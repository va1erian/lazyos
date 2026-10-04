//! A read/write ext2 filesystem (issue #99), shared by the kernel and the host
//! image build (`docs/filesystem-plan.md` F2).
//!
//! # On-disk shape
//!
//! ext2 is the classic Unix filesystem: a superblock describes the volume, a
//! table of group descriptors splits it into block groups, and each group owns
//! a block bitmap, an inode bitmap, and an inode table. An inode maps file data
//! through fifteen 32-bit block slots: twelve direct, then single, double, and
//! triple indirect. Directories are just files whose contents are a sequence of
//! variable-length name entries.
//!
//! This driver speaks revision 0/1 ext2 with 1 KiB, 2 KiB, or 4 KiB blocks. The
//! kernel wraps it in its VFS `Filesystem` trait (`kernel/src/fs/ext2/fsimpl.rs`),
//! which is the first writable filesystem the platform plan asks for
//! (`docs/platform-plan.md` section 4.4); the host build drives it directly
//! through [`Ext2::mkdir_p`], [`Ext2::write_file`] and [`Ext2::remove_tree`].
//! It touches the outside world through three seams only: a [`BlockIo`] device,
//! a [`Clock`], and its own error and metadata types. Long operations call
//! [`BlockIo::pace`] once per unit of work (block, pending free, writeback
//! request), where a host running the library with interrupts off (the
//! kernel) takes them. It implements:
//!
//! * superblock and group descriptors, with the free counters kept in sync;
//! * inode and directory operations: `lookup`, `create`, `mkdir`, `unlink`,
//!   `rename`, `readdir`, and `stat` (through `lookup`);
//! * block allocation from the group bitmaps with per-group accounting;
//! * reads and writes through the direct, single-, double- and triple-indirect
//!   block maps, with sparse holes, and `truncate` (grow and shrink);
//! * timestamps stamped from the [`Clock`] (UTC wall time), and `setattr` for
//!   `chmod`/`chown`/`utimensat` (`attr.rs`);
//! * a clean/dirty superblock state (`s_state`) and [`Ext2::flush`], which
//!   flushes the device and then marks the volume clean (see `state.rs`).
//!
//! # Deliberate limits
//!
//! * No journal and no guessing: feature bits that change the layout we do not
//!   understand (extents, 64-bit, htree, ...) are rejected in [`Ext2::open`].
//! * Files are capped at [`MAX_FILE_SIZE`] (2 GiB - 1); directories may use
//!   only the direct and single-indirect blocks.
//! * No symlinks or device nodes yet: only files and directories are modelled,
//!   so an inode with any other type bits answers [`Ext2Error::NotSupported`].
//! * Directories are scanned linearly; an indexed (htree) directory can be read
//!   but not changed ([`Ext2Error::NotSupported`]), because a linear scan cannot
//!   keep its index coherent.
//! * Only one block-sized buffer is live per helper and every loop is bounded
//!   by a geometry field, so a malformed image cannot hang or panic the host.
//!
//! # Locking
//!
//! An instance is `Sync` so several tasks may call it concurrently, but every
//! entry point takes one private mutex: a write updates an inode, a bitmap, and
//! the superblock, and a concurrent lookup must never observe half of that.
//!
//! # Caching
//!
//! [`Ext2::open`] reads and writes the device directly, one block at a time,
//! in the order the operations above describe. [`Ext2::open_cached`] puts a
//! write-back block cache in between (`cache/`): writes stay in memory until a
//! commit writes them back in a crash-safe phase order, coalesced into large
//! requests, and frees wait for that commit (`commit.rs`). The kernel and the
//! host image build mount through the cache; the crash-ordering tests use the
//! direct path. `docs/architecture/block-cache.md` has the crash semantics.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::min;
use core::sync::atomic::AtomicBool;
use spin::Mutex;

mod attr;
mod blocks;
mod cache;
mod capacity;
mod commit;
mod dir;
mod error;
mod file_io;
mod format;
mod geometry;
mod handle;
mod indirect;
mod io;
mod layout;
mod links;
mod open;
mod orphans;
mod populate;
mod read_run;
mod readdir;
mod rename;
mod rename_file;
mod repair;
mod rmdir;
mod state;
mod truncate;
mod types;

#[cfg(any(test, feature = "check"))]
extern crate std;
#[cfg(any(test, feature = "check"))]
pub mod check;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
#[cfg(any(test, feature = "fuzz"))]
pub mod memio;
#[cfg(any(test, feature = "check"))]
mod recover;
#[cfg(test)]
mod tests;

pub use cache::memory::{CacheConfig, CacheMemory, CachePage, HeapMemory, CACHE_PAGE_SIZE};
pub use cache::CacheStats;
pub use error::{zeroed, BlockIo, Ext2Error, IoError, SECTOR_SIZE};
pub use format::format;
pub use geometry::Geometry;
pub use handle::FileHandle;
pub use layout::MAX_FILE_SIZE;
pub use orphans::{OrphanReport, MAX_SCAN_DIRS};
#[cfg(any(test, feature = "check"))]
pub use recover::Recovery;
pub use repair::{Listed, RepairError, RepairReport, LISTED};

/// The reserved name prefix of files parked by an unlink-while-open
/// (`.unlinked-<n>`): what [`Ext2::reclaim_orphans`] is handed by the kernel
/// and by `Ext2::recover` (the image build), so both delete exactly the same names.
pub const ORPHAN_PREFIX: &str = ".unlinked-";
pub use types::{
    AttrChange, DirEntry, FileKind, FsStats, InodeMeta, Owner, Times, S_IFDIR, S_IFMT, S_IFREG,
};

use layout::*;

/// UTC seconds since the Unix epoch, supplied by whoever opens the volume.
pub type Clock = fn() -> i64;

/// One group descriptor, in host order.
#[derive(Clone, Copy)]
struct GroupDesc {
    block_bitmap: u32,
    inode_bitmap: u32,
    inode_table: u32,
    free_blocks: u16,
    free_inodes: u16,
    used_dirs: u16,
}

/// A mounted ext2 volume. See the module docs for the supported surface.
pub struct Ext2 {
    io: Box<dyn BlockIo>,
    /// Where inode timestamps come from.
    clock: Clock,
    /// 1024, 2048, or 4096 bytes.
    block_size: u32,
    /// Device sectors per filesystem block.
    sectors_per_block: u32,
    inodes_count: u32,
    blocks_count: u32,
    first_data_block: u32,
    blocks_per_group: u32,
    inodes_per_group: u32,
    inode_size: u16,
    inodes_per_block: u32,
    /// Single-indirect pointers that fit in one block.
    ptrs_per_block: u32,
    first_ino: u32,
    groups: u32,
    /// Block holding the first group descriptor (the table may span blocks).
    gdt_block: u64,
    /// Whether directory entries carry a file-type byte.
    has_file_type: bool,
    /// Whether regular-file sizes use `i_dir_acl` as the high 32 bits.
    has_large_file: bool,
    /// The device cannot be written: reads work, mutations answer `EROFS`.
    read_only: bool,
    /// `s_state` as found at mount. A clean sync restores exactly this, so a
    /// volume that was already unclean (or errored) stays flagged until an
    /// fsck, rather than being blessed by our own clean shutdown.
    mount_state: u16,
    /// `s_uuid` and `s_volume_name` (NUL-padded) from the superblock.
    uuid: [u8; 16],
    label: [u8; 16],
    /// Whether the on-disk `s_state` currently says clean. Only touched under
    /// `lock`; see `state.rs` for the ordering rules.
    clean: AtomicBool,
    /// The write-back block cache, when the host asked for one
    /// ([`Ext2::open_cached`]; `cache/mod.rs`). Only touched under `lock`.
    cache: Option<Mutex<cache::BlockCache>>,
    /// Whether frees wait for the next commit (`commit.rs`); on with a cache.
    defer_frees: bool,
    pending: Mutex<commit::Pending>,
    /// A write-back failed this mount: `s_state` will carry the error bit.
    errored: AtomicBool,
    /// ... and no `flush` caller has been told yet.
    error_unreported: AtomicBool,
    /// Serialises every operation; see the module docs.
    lock: Mutex<()>,
    /// Called between the steps of a long operation ([`Ext2::set_pause`]).
    pause: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Ext2 {
    /// The superblock's volume UUID (`s_uuid`), as stored.
    pub fn uuid(&self) -> [u8; 16] {
        self.uuid
    }

    /// The superblock's volume label (`s_volume_name`), NUL-padded.
    pub fn label(&self) -> [u8; 16] {
        self.label
    }

    /// Whether the device cannot be written: reads work, mutations answer
    /// [`Ext2Error::ReadOnly`].
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Bytes per filesystem block.
    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    /// The superblock's free-block counter (the future `statfs` surface), plus
    /// blocks freed but not yet committed (`commit.rs`).
    pub fn free_blocks(&self) -> Result<u32, Ext2Error> {
        let _guard = self.lock.lock();
        let mut raw = [0u8; 1024];
        self.read_super_raw(&mut raw)?;
        Ok(le32(&raw, SB_FREE_BLOCKS).saturating_add(self.pending_frees().0))
    }

    /// The superblock's free-inode counter (plus inodes freed but not yet
    /// committed, like [`Ext2::free_blocks`]).
    pub fn free_inodes(&self) -> Result<u32, Ext2Error> {
        let _guard = self.lock.lock();
        let mut raw = [0u8; 1024];
        self.read_super_raw(&mut raw)?;
        Ok(le32(&raw, SB_FREE_INODES).saturating_add(self.pending_frees().1))
    }

    /// The on-disk link count of `path`'s inode; the diagnostic the tests use
    /// to check directory bookkeeping (`.`/`..` links) after renames.
    pub fn link_count(&self, path: &str) -> Result<u16, Ext2Error> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        Ok(le16(&self.read_inode(ino)?, INO_LINKS))
    }

    /// The physical block backing logical `index` of `path` (`0` for a hole).
    /// This is the diagnostic surface the tests use to see allocation reuse.
    pub fn mapped_block(&self, path: &str, index: u32) -> Result<u32, Ext2Error> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        let inode = self.read_inode(ino)?;
        if kind_from_mode(le16(&inode, INO_MODE)) != Some(FileKind::File) {
            return Err(Ext2Error::IsDir);
        }
        self.block_map(&inode, index)
    }

    /// Make everything written so far durable and mark the volume clean.
    /// This is the umount/fsync/shutdown surface (`state.rs` has the ordering).
    pub fn flush(&self) -> Result<(), Ext2Error> {
        self.sync_volume()
    }

    /// Have `hook` called between the steps of long operations (freeing a
    /// large file's blocks, applying a commit's frees, scanning a directory
    /// block, every 16 blocks written, every run read), with the volume lock
    /// held: a kernel whose calls run with interrupts off lets them in there
    /// (`kernel/src/fs/ext2/volio.rs`). The hook must not call back into the
    /// volume.
    pub fn set_pause(&mut self, hook: Box<dyn Fn() + Send + Sync>) {
        self.pause = Some(hook);
    }

    /// A point where a long operation may pause (see [`Ext2::set_pause`]).
    pub(crate) fn pause_point(&self) {
        if let Some(hook) = &self.pause {
            hook();
        }
    }

    /// The current time as an inode field holds it: the [`Clock`], clamped to
    /// the 32-bit range an inode can store (see [`attr::disk_time`]).
    pub(crate) fn now(&self) -> u32 {
        attr::disk_time((self.clock)())
    }
}
