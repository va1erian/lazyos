//! The kernel's ext2 volume: a thin adapter over `libs/ext2fs`.
//!
//! The driver itself (superblock, block maps, directories, orphan reclaim, the
//! formatter) lives in the library so the host image build and the kernel run
//! the same code (`docs/filesystem-plan.md` F2). What stays here is everything
//! that needs the kernel:
//!
//! * [`BlockIo`](ext2fs::BlockIo) for a registered [`BlockDevice`], which also
//!   reports the first failure of each kind on serial;
//! * the VFS clock, handed to the library at open;
//! * the serial lines a mount prints (`was not cleanly unmounted`, orphan
//!   reclaim failures), built from what the library returns;
//! * the [`Filesystem`](super::vfs::Filesystem) impl (`fsimpl.rs`), which also
//!   owns the rule that a reserved `.unlinked-*` name is deleted inode-first
//!   ([`hidden`]).
//!
//! The library error maps 1:1 onto [`FsError`] in the single `From` below.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, Ordering};

use super::hidden;
use super::vfs::FsError;
use crate::block::{BlockDevice, BlockError, SECTOR_SIZE};

mod cache;
mod fsimpl;
mod volio;

pub use cache::cache_frames;

/// A mounted ext2 volume. See `libs/ext2fs` for the supported surface.
///
/// `gate` serialises every call into the library. The library's own lock is
/// a plain spin lock, and a call may park inside it: waiting for a user-space
/// block provider (a USB stick), or for virtio-blk (`block::iowait`). Both
/// mount tables reach the same volume, so a second task must meet a lock that
/// yields (`task::relax`) before it can reach the library's ([`volio`]).
pub struct Ext2 {
    volume: ext2fs::Ext2,
    /// The registry name of the device, for the log lines.
    device: &'static str,
    gate: volio::Gate,
    /// The device as the library reaches it, and whether the gate's holder
    /// may sleep in it.
    io: Arc<volio::VolumeIo>,
}

impl Ext2 {
    /// Probe `device` for an ext2 superblock and mount it, reading and
    /// writing the device directly. Any malformed or unsupported image is
    /// refused with a friendly [`FsError`]; nothing here trusts the disk. The
    /// tests that judge the device's bytes after every write use this, and so
    /// does a volume on a user-space provider disk (`mounts::open_ext2`);
    /// other mounts go through [`Ext2::open_cached`].
    pub fn open(device: &'static dyn BlockDevice) -> Result<Ext2, FsError> {
        Ext2::mount(device, None)
    }

    /// [`Ext2::open`] through the write-back block cache (`cache.rs`).
    pub fn open_cached(device: &'static dyn BlockDevice) -> Result<Ext2, FsError> {
        Ext2::mount(device, Some(cache::config()))
    }

    fn mount(
        device: &'static dyn BlockDevice,
        config: Option<ext2fs::CacheConfig>,
    ) -> Result<Ext2, FsError> {
        // Every block device in this tree speaks 512-byte sectors, which is
        // what the library's `BlockIo` assumes; another size is refused.
        if device.sector_size() != SECTOR_SIZE {
            return Err(FsError::NotSupported);
        }
        let shared = volio::VolumeIo::new(device);
        let io = Box::new(volio::SharedIo(shared.clone()));
        let mut volume = match config {
            Some(config) => ext2fs::Ext2::open_cached(io, super::vfs::now, config)?,
            None => ext2fs::Ext2::open(io, super::vfs::now)?,
        };
        volume.set_pause(volio::pause_hook(&shared));
        // A mount never repairs anything (no fsck here); it only makes the
        // situation visible. The flag survives our own clean shutdowns: only
        // a check clears it (the image build's `Ext2::recover`).
        if !volume.was_clean_at_mount() {
            serial_println!(
                "ext2: {} was not cleanly unmounted (unclean stop)",
                device.name()
            );
        }
        if volume.journal_recovered() {
            serial_println!("ext2: {} replayed its journal", device.name());
        }
        if volume.had_errors_at_mount() {
            serial_println!("ext2: {} has recorded filesystem errors", device.name());
        }
        Ok(Ext2 {
            volume,
            device: device.name(),
            gate: volio::Gate::new(()),
            io: shared,
        })
    }

    /// The superblock's volume UUID (`s_uuid`), as stored.
    pub fn uuid(&self) -> [u8; 16] {
        self.volume.uuid()
    }

    /// The superblock's volume label (`s_volume_name`), NUL-padded.
    pub fn label(&self) -> [u8; 16] {
        self.volume.label()
    }

    /// Bytes per filesystem block.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // diagnostics/tests
    pub fn block_size(&self) -> u32 {
        self.volume.block_size()
    }

    /// The superblock's free-block counter (the future `statfs` surface).
    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn free_blocks(&self) -> Result<u32, FsError> {
        let _gate = self.enter();
        Ok(self.volume.free_blocks()?)
    }

    /// The superblock's free-inode counter.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn free_inodes(&self) -> Result<u32, FsError> {
        let _gate = self.enter();
        Ok(self.volume.free_inodes()?)
    }

    /// The on-disk link count of `path`'s inode; the diagnostic the tests use
    /// to check directory bookkeeping (`.`/`..` links) after renames.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn link_count(&self, path: &str) -> Result<u16, FsError> {
        let _gate = self.enter();
        Ok(self.volume.link_count(path)?)
    }

    /// The physical block backing logical `index` of `path` (`0` for a hole).
    /// This is the diagnostic surface the tests use to see allocation reuse.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn mapped_block(&self, path: &str, index: u32) -> Result<u32, FsError> {
        let _gate = self.enter();
        Ok(self.volume.mapped_block(path, index)?)
    }

    /// Make everything written so far durable and mark the volume clean.
    /// This is the umount/fsync/shutdown surface.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // the trait method is the caller
    pub fn flush(&self) -> Result<(), FsError> {
        let _gate = self.enter();
        Ok(self.volume.flush()?)
    }

    /// Whether the volume was flagged clean when it was mounted.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // diagnostics/tests
    pub fn was_clean_at_mount(&self) -> bool {
        self.volume.was_clean_at_mount()
    }

    /// Delete every parked `.unlinked-*` orphan and return how many were
    /// reclaimed. Does nothing on a read-only device or a cleanly unmounted
    /// volume; a file that cannot be reclaimed is reported and left for the
    /// next mount.
    pub fn reclaim_orphans(&self) -> usize {
        let _gate = self.enter();
        let report = self.volume.reclaim_orphans(hidden::PREFIX);
        if report.scan_truncated {
            serial_println!(
                "ext2: orphan scan stopped at {} directories",
                ext2fs::MAX_SCAN_DIRS
            );
        }
        for (path, error) in &report.failed {
            serial_println!(
                "ext2: could not reclaim {path}: {:?}",
                FsError::from(*error)
            );
        }
        report.reclaimed
    }
}

impl From<ext2fs::Ext2Error> for FsError {
    /// One variant for one: the library names its failures after the POSIX
    /// errors the VFS already carries. A failing device (`Io`) has always
    /// reached callers as `Invalid`, the "corrupt or missing backing store"
    /// answer, so it keeps doing so.
    fn from(error: ext2fs::Ext2Error) -> FsError {
        use ext2fs::Ext2Error as E;
        match error {
            E::NotFound => FsError::NotFound,
            E::Exists => FsError::Exists,
            E::NotDir => FsError::NotDir,
            E::IsDir => FsError::IsDir,
            E::NotEmpty => FsError::NotEmpty,
            E::ReadOnly => FsError::ReadOnly,
            E::Invalid | E::Io => FsError::Invalid,
            E::NoSpace => FsError::NoSpace,
            E::NameTooLong => FsError::NameTooLong,
            E::NotSupported => FsError::NotSupported,
        }
    }
}

/// Map a block-layer failure onto the library's two: a write to a read-only
/// device keeps its friendly answer, everything else is a failed device.
fn io_error(error: BlockError) -> ext2fs::IoError {
    log_block_error(error);
    match error {
        BlockError::ReadOnly => ext2fs::IoError::ReadOnly,
        _ => ext2fs::IoError::Failed,
    }
}

/// Report the first failure of each kind on serial. The library folds them
/// all into one error, which once hid a flaky virtio device behind "invalid
/// argument".
fn log_block_error(error: BlockError) {
    static SEEN: AtomicU32 = AtomicU32::new(0);
    let bit = 1u32 << (error as u32 % 32);
    if SEEN.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
        serial_println!(
            "ext2: block layer error {:?} (reported once per kind)",
            error
        );
    }
}
