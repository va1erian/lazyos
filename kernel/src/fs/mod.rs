//! Filesystem: the VFS core, a read-only FAT12/16 volume on the boot disk, a
//! read/write ext2 driver (issue #99), and a ramfs scratch mount at `/tmp`
//! (issue #98).
//!
//! The FAT reader, ext2 (issue #99), and the in-memory ramfs all implement
//! [`vfs::Filesystem`]; [`init`] mounts the boot volume at `/` (FAT first,
//! then ext2 if the volume carries it) and ramfs at `/tmp`. The helpers below
//! are the kernel-side entry points: the native loader reads through `read`,
//! and the Linux ABI fd layer uses the `abi_*` surface. They stamp the current
//! task's credentials through [`vfs::Id`], so permission checks apply to every
//! read, not just the Linux syscalls.
//!
//! # Two mount tables
//!
//! Native tasks use the raw [`FS`] table above: the boot volume exactly as the
//! backend presents it. The Linux ABI gets its own table ([`ABI_FS`]) where `/`
//! is a copy-up [`overlay::Overlay`] over the boot volume, so
//! `open(O_CREAT)`/`mkdir`/`rename`/`unlink` succeed without writing the FAT
//! image (issue #136). The overlay's upper layer is in-memory and discarded on
//! reboot; native tasks do not see ABI writes. `/tmp` is one shared ramfs
//! mounted in both tables, so scratch files are visible to both.
//!
//! # The data volume
//!
//! A second block device carrying ext2 is mounted read/write at `/data` in both
//! tables ([`mount_data_volume`]); it is the durable store. [`sync_all`] is the
//! shutdown hook that makes it consistent on disk.

pub mod ext2;
pub mod fallible;
pub mod fat;
pub mod openfile;
pub mod overlay;
pub mod ramfs;
pub mod vfs;

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use crate::block;
use vfs::{DirEntry, Filesystem, FsError, Id, Meta, Vfs};

/// The native kernel VFS: mount table, caches, and whether the boot volume
/// mounted. `None` until [`init`] runs.
static FS: Mutex<Option<(Vfs, bool)>> = Mutex::new(None);

/// The Linux ABI's mount table: a copy-up overlay over the boot volume at `/`
/// (or a plain ramfs when no volume mounted) plus a ramfs at `/tmp`. Built by
/// [`init`]; `None` until the boot volumes are mounted.
static ABI_FS: Mutex<Option<Vfs>> = Mutex::new(None);

/// Pick the volume that becomes `/`: the first FAT12/16 volume on any device,
/// and only when there is none, the first ext2 one (issue #99). FAT is the
/// shipped boot format, so it wins regardless of enumeration order; otherwise
/// an ext2 data disk enumerated ahead of the boot disk (an IDE data disk before
/// a virtio boot disk, say) would take `/` and leave the boot volume unmounted.
pub(crate) fn select_root(
    devices: &[&'static dyn block::BlockDevice],
) -> Option<(Arc<dyn Filesystem>, &'static str)> {
    let fat = devices.iter().find_map(|device| {
        let volume = fat::Fat16::open(*device)?;
        Some((Arc::new(volume) as Arc<dyn Filesystem>, device.name()))
    });
    fat.or_else(|| {
        devices.iter().find_map(|device| {
            let volume = ext2::Ext2::open(*device).ok()?;
            Some((Arc::new(volume) as Arc<dyn Filesystem>, device.name()))
        })
    })
}

/// Probe the block layer, mount the boot volume at `/`, and a fresh ramfs at
/// `/tmp`. Returns whether a filesystem volume was found (the ramfs mount
/// always succeeds). Idempotent: a second call reports the first call's
/// boot-volume result without remounting.
///
/// Device selection runs through the block registry (issue #100); see
/// [`select_root`] for how `/` is chosen. Both readers open the device they are
/// handed and keep that handle (issue #244), so a probe on one disk cannot
/// read from another.
pub fn init() -> bool {
    let mut global = FS.lock();
    if let Some((_, mounted)) = global.as_ref() {
        return *mounted;
    }
    block::init();
    let mut vfs = Vfs::new();
    let devices = block::devices();
    let (root, root_device) = match select_root(&devices) {
        Some((volume, name)) => (Some(volume), Some(name)),
        None => (None, None),
    };
    let mounted = root.is_some();
    if let Some(volume) = &root {
        let _ = vfs.mount("/", Arc::clone(volume));
    } else {
        serial_println!("fs: no FAT or ext2 volume on any block device");
    }
    // `/tmp` is the scratch filesystem: writable, in memory, and discarded on
    // reboot. Mounting it even when FAT is missing keeps the VFS usable. The
    // ABI table below mounts the same instance so both views agree.
    let tmp: Arc<dyn Filesystem> = Arc::new(ramfs::RamFs::new());
    let _ = vfs.mount("/tmp", Arc::clone(&tmp));
    for (point, name) in vfs.mounts() {
        crate::serial_println!("fs: mounted {name} at {point}");
    }
    let data = mount_data_volume(&mut vfs, root_device, &devices);

    // The Linux ABI sees a writable root: a copy-up overlay over the read-only
    // boot volume. Upper-layer contents live in memory and are discarded on
    // reboot; native tasks keep the raw mounts above.
    let mut abi = Vfs::new();
    let abi_root: Arc<dyn Filesystem> = match root {
        Some(volume) => Arc::new(overlay::Overlay::new(volume)),
        None => Arc::new(ramfs::RamFs::new()),
    };
    let _ = abi.mount("/", abi_root);
    let _ = abi.mount("/tmp", tmp);
    if let Some(volume) = data {
        let _ = abi.mount("/data", volume);
    }
    for (point, name) in abi.mounts() {
        crate::serial_println!("fs: abi mounted {name} at {point}");
    }
    *ABI_FS.lock() = Some(abi);

    *global = Some((vfs, mounted));
    mounted
}

/// Mount the first ext2 volume that is not the root device at `/data`.
///
/// This is the one place the data volume is mounted (there is no mount
/// syscall). A missing device is not an error: a session without a data disk
/// simply has no `/data`. Every volume is opened at most once, and a device
/// that fails to open as ext2 is skipped, never guessed at. Returns the
/// mounted volume so the caller can share it with the Linux ABI table.
pub(crate) fn mount_data_volume(
    vfs: &mut Vfs,
    root_device: Option<&str>,
    devices: &[&'static dyn block::BlockDevice],
) -> Option<Arc<dyn Filesystem>> {
    for device in devices {
        if Some(device.name()) == root_device {
            continue;
        }
        let Ok(volume) = ext2::Ext2::open(*device) else {
            continue;
        };
        let volume: Arc<dyn Filesystem> = Arc::new(volume);
        if vfs.mount(DATA_MOUNT, Arc::clone(&volume)).is_err() {
            return None;
        }
        crate::serial_println!("fs: mounted {} at {DATA_MOUNT}", device.name());
        if !device.is_writable() {
            crate::serial_println!("fs: {DATA_MOUNT} is read-only (device cannot be written)");
        }
        return Some(volume);
    }
    None
}

/// Where the durable ext2 data volume lives.
const DATA_MOUNT: &str = "/data";

/// Flush every mounted filesystem to stable storage and mark clean volumes
/// clean. Called on the way to power-off/reboot; a filesystem that fails is
/// reported after the others have still been flushed.
pub fn sync_all() -> Result<(), FsError> {
    with(|vfs| vfs.sync_all()).unwrap_or(Ok(()))
}

/// Mount the filesystem on a registered block device at `point`. This is the
/// `mount <dev>` surface: every device is tried as FAT first (the shipped
/// read-only format) and then as ext2. A device carrying neither returns [`FsError::NotSupported`].
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // the `mount <dev>` surface
pub fn mount_device(point: &str, device: &str) -> Result<(), FsError> {
    let device = block::device(device).ok_or(FsError::NotFound)?;
    // A FAT volume is bound to the device it was opened from, so any device
    // may carry one (issue #244); a non-FAT device fails the BPB checks.
    if let Some(volume) = fat::Fat16::open(device) {
        return with(|vfs| vfs.mount(point, Arc::new(volume))).unwrap_or(Err(FsError::NotFound));
    }
    match ext2::Ext2::open(device) {
        Ok(volume) => {
            with(|vfs| vfs.mount(point, Arc::new(volume))).unwrap_or(Err(FsError::NotFound))
        }
        Err(_) => Err(FsError::NotSupported),
    }
}

/// Run `f` against the global VFS, if it is mounted.
fn with<T>(f: impl FnOnce(&mut Vfs) -> T) -> Option<T> {
    FS.lock().as_mut().map(|(vfs, _)| f(vfs))
}

/// Read a whole file as the current task (permission-checked).
pub fn read(name: &str) -> Option<Vec<u8>> {
    let id = Id::current();
    with(|vfs| vfs.read_file(id, name).ok()).flatten()
}

/// List the root directory as `(name, is_dir, size)`. Only used by diagnostics
/// today; the Linux layer lists directories through [`abi_readdir`].
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn list() -> Vec<(String, bool, u32)> {
    let id = Id::current();
    let entries = match with(|vfs| vfs.readdir(id, "/")) {
        Some(Ok(entries)) => entries,
        _ => return Vec::new(),
    };
    entries
        .into_iter()
        .map(|entry| {
            let size = with(|vfs| vfs.stat(id, &entry.name).ok().map(|meta| meta.size))
                .flatten()
                .unwrap_or(0);
            (entry.name, entry.kind == vfs::FileKind::Dir, size as u32)
        })
        .collect()
}

/// Metadata through the native VFS (permission-checked; `__`-free results for
/// callers to map to errno).
pub fn vfs_stat(id: Id, path: &str) -> Result<Meta, FsError> {
    with(|vfs| vfs.stat(id, path)).unwrap_or(Err(FsError::NotFound))
}

/// Read a whole file through the native VFS.
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // used by tests/diagnostics
pub fn vfs_read(id: Id, path: &str) -> Result<Vec<u8>, FsError> {
    with(|vfs| vfs.read_file(id, path)).unwrap_or(Err(FsError::NotFound))
}

/// Write at an offset through the VFS (used by tests and future writers).
pub fn vfs_write(id: Id, path: &str, offset: u64, data: &[u8]) -> Result<usize, FsError> {
    with(|vfs| vfs.write(id, path, offset, data)).unwrap_or(Err(FsError::NotFound))
}

/// Create a regular file through the VFS.
pub fn vfs_create(id: Id, path: &str, mode: u16) -> Result<Meta, FsError> {
    with(|vfs| vfs.create(id, path, mode)).unwrap_or(Err(FsError::NotFound))
}

/// Create a directory through the VFS.
pub fn vfs_mkdir(id: Id, path: &str, mode: u16) -> Result<Meta, FsError> {
    with(|vfs| vfs.mkdir(id, path, mode)).unwrap_or(Err(FsError::NotFound))
}

/// Truncate or extend a regular file through the VFS.
pub fn vfs_truncate(id: Id, path: &str, size: u64) -> Result<(), FsError> {
    with(|vfs| vfs.truncate(id, path, size)).unwrap_or(Err(FsError::NotFound))
}

/// List a directory through the native VFS (permission-checked).
pub fn vfs_readdir(id: Id, path: &str) -> Result<Vec<DirEntry>, FsError> {
    with(|vfs| vfs.readdir(id, path)).unwrap_or(Err(FsError::NotFound))
}

/// Remove an empty directory through the native VFS.
pub fn vfs_rmdir(id: Id, path: &str) -> Result<(), FsError> {
    with(|vfs| vfs.rmdir(id, path)).unwrap_or(Err(FsError::NotFound))
}

/// Remove a regular file through the VFS.
pub fn vfs_unlink(id: Id, path: &str) -> Result<(), FsError> {
    with(|vfs| vfs.unlink(id, path)).unwrap_or(Err(FsError::NotFound))
}

/// Rename within one mount through the VFS.
pub fn vfs_rename(id: Id, from: &str, to: &str) -> Result<(), FsError> {
    with(|vfs| vfs.rename(id, from, to)).unwrap_or(Err(FsError::NotFound))
}

/// Flush the filesystem holding `path` to stable storage (`fsync(2)`).
pub fn vfs_flush(id: Id, path: &str) -> Result<(), FsError> {
    with(|vfs| vfs.flush(id, path)).unwrap_or(Err(FsError::NotFound))
}

/// The global creation mask.
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // read back by tests/diagnostics
pub fn vfs_umask() -> u16 {
    with(|vfs| vfs.umask()).unwrap_or(0)
}

/// Set the global creation mask, returning the previous one (`umask(2)`).
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // used by tests/diagnostics
pub fn vfs_set_umask(mask: u16) -> u16 {
    with(|vfs| vfs.set_umask(mask)).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Linux ABI surface (issue #136)
//
// These mirror the native helpers above against the ABI mount table, where the
// root is a copy-up overlay (see the module docs). The Linux syscall layer is
// the only caller.
// ---------------------------------------------------------------------------

/// Run `f` against the Linux ABI's VFS, if [`init`] has built it.
fn abi_with<T>(f: impl FnOnce(&mut Vfs) -> T) -> Option<T> {
    ABI_FS.lock().as_mut().map(f)
}

/// Mount points of the Linux ABI table, in mount order, with each
/// filesystem's short name (which ends in `(ro)` for a read-only mount).
pub fn abi_mounts() -> Vec<(String, &'static str)> {
    abi_with(|vfs| vfs.mounts()).unwrap_or_default()
}

/// Metadata through the Linux ABI VFS (permission-checked).
pub fn abi_stat(id: Id, path: &str) -> Result<Meta, FsError> {
    abi_with(|vfs| vfs.stat(id, path)).unwrap_or(Err(FsError::NotFound))
}

/// Access check on one Linux ABI path.
pub fn abi_check(id: Id, path: &str, mask: u8) -> Result<Meta, FsError> {
    abi_with(|vfs| vfs.check(id, path, mask)).unwrap_or(Err(FsError::NotFound))
}

/// Read a whole file through the Linux ABI VFS.
pub fn abi_read(id: Id, path: &str) -> Result<Vec<u8>, FsError> {
    abi_with(|vfs| vfs.read_file(id, path)).unwrap_or(Err(FsError::NotFound))
}

/// List a directory through the Linux ABI VFS.
pub fn abi_readdir(id: Id, path: &str) -> Result<Vec<DirEntry>, FsError> {
    abi_with(|vfs| vfs.readdir(id, path)).unwrap_or(Err(FsError::NotFound))
}

/// Write at an offset through the Linux ABI VFS.
pub fn abi_write(id: Id, path: &str, offset: u64, data: &[u8]) -> Result<usize, FsError> {
    abi_with(|vfs| vfs.write(id, path, offset, data)).unwrap_or(Err(FsError::NotFound))
}

/// Truncate a file through the Linux ABI VFS (`O_TRUNC`).
pub fn abi_truncate(id: Id, path: &str, size: u64) -> Result<(), FsError> {
    abi_with(|vfs| vfs.truncate(id, path, size)).unwrap_or(Err(FsError::NotFound))
}

/// Create a regular file through the Linux ABI VFS.
pub fn abi_create(id: Id, path: &str, mode: u16) -> Result<Meta, FsError> {
    abi_with(|vfs| vfs.create(id, path, mode)).unwrap_or(Err(FsError::NotFound))
}

/// Create a directory through the Linux ABI VFS.
pub fn abi_mkdir(id: Id, path: &str, mode: u16) -> Result<Meta, FsError> {
    abi_with(|vfs| vfs.mkdir(id, path, mode)).unwrap_or(Err(FsError::NotFound))
}

/// Read up to `buf.len()` bytes at `offset` through the Linux ABI VFS.
pub fn abi_read_at(id: Id, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
    abi_with(|vfs| vfs.read(id, path, offset, buf)).unwrap_or(Err(FsError::NotFound))
}

/// Remove a regular file through the Linux ABI VFS. A file that a descriptor
/// still has open loses its name but keeps its data until the last close
/// ([`openfile`]).
pub fn abi_unlink(id: Id, path: &str) -> Result<(), FsError> {
    if openfile::unlink_open(id, path)? {
        return Ok(());
    }
    abi_unlink_raw(id, path)
}

/// Delete a name outright, with no regard for open descriptors (the hidden
/// entry of an unlinked file is deleted this way when its last one closes).
fn abi_unlink_raw(id: Id, path: &str) -> Result<(), FsError> {
    abi_with(|vfs| vfs.unlink(id, path)).unwrap_or(Err(FsError::NotFound))
}

/// Flush the filesystem holding `path` (`fsync`/`fdatasync`/`syncfs`).
pub fn abi_flush(id: Id, path: &str) -> Result<(), FsError> {
    abi_with(|vfs| vfs.flush(id, path)).unwrap_or(Err(FsError::NotFound))
}

/// Flush every mount of the Linux ABI table (`sync`). The `/tmp` ramfs is the
/// same instance in both tables and the overlay has nothing to flush, so this
/// is the durability of the data volume; one failing mount does not stop the
/// others.
pub fn abi_sync_all() -> Result<(), FsError> {
    abi_with(|vfs| vfs.sync_all()).unwrap_or(Ok(()))
}

/// Capacity of the filesystem holding `path` (`statfs`).
pub fn abi_statfs(id: Id, path: &str) -> Result<vfs::StatFs, FsError> {
    abi_with(|vfs| vfs.statfs(id, path)).unwrap_or(Err(FsError::NotFound))
}

/// Whether `path` lives on the durable data volume, whose files a Linux
/// descriptor reads and writes in place ([`openfile::OpenFile`]) instead of
/// through a snapshot. False when no data volume is mounted: `/data` is then
/// an ordinary directory of the copy-up root.
pub fn abi_persistent(path: &str) -> bool {
    abi_with(|vfs| vfs.mount_point(path)).flatten().as_deref() == Some(DATA_MOUNT)
}

/// Remove an empty directory through the Linux ABI VFS.
pub fn abi_rmdir(id: Id, path: &str) -> Result<(), FsError> {
    abi_with(|vfs| vfs.rmdir(id, path)).unwrap_or(Err(FsError::NotFound))
}

/// Rename within one mount through the Linux ABI VFS. Open files follow their
/// name, and one that the rename replaces is unlinked, not destroyed
/// ([`openfile`]).
pub fn abi_rename(id: Id, from: &str, to: &str) -> Result<(), FsError> {
    let same = vfs::Path::parse(from) == vfs::Path::parse(to);
    let displaced = if same {
        None
    } else {
        openfile::displace(id, to)?
    };
    match abi_rename_raw(id, from, to) {
        Ok(()) => {
            openfile::retarget(from, to);
            Ok(())
        }
        Err(error) => {
            if let Some(displaced) = displaced {
                displaced.restore();
            }
            Err(error)
        }
    }
}

/// Rename a name with no regard for open descriptors.
fn abi_rename_raw(id: Id, from: &str, to: &str) -> Result<(), FsError> {
    abi_with(|vfs| vfs.rename(id, from, to)).unwrap_or(Err(FsError::NotFound))
}

/// Set the ABI creation mask, returning the previous one (`umask(2)`).
pub fn abi_set_umask(mask: u16) -> u16 {
    abi_with(|vfs| vfs.set_umask(mask)).unwrap_or(0)
}

/// Install a fresh Linux ABI mount table backed entirely by ramfs (issue
/// #229's leak test): the test suite boots without [`init`] having mounted a
/// boot volume, so the ABI table would otherwise be `None` and no path could
/// reach `execve`'s load path.
#[cfg(lazyos_tests)]
pub fn install_abi_ramfs_for_test() {
    let mut abi = Vfs::new();
    let _ = abi.mount("/", Arc::new(ramfs::RamFs::new()));
    let _ = abi.mount("/tmp", Arc::new(ramfs::RamFs::new()));
    *ABI_FS.lock() = Some(abi);
}

/// Swap in a Linux ABI table whose `/data` is `volume` (over a ramfs root and
/// `/tmp`), returning the table it replaced so a test can put it back with
/// [`restore_abi_for_test`]. This is what a boot with a data disk builds,
/// without needing a second block device.
#[cfg(lazyos_tests)]
pub fn install_abi_data_for_test(volume: Arc<dyn Filesystem>) -> Option<Vfs> {
    let mut abi = Vfs::new();
    let _ = abi.mount("/", Arc::new(ramfs::RamFs::new()));
    let _ = abi.mount("/tmp", Arc::new(ramfs::RamFs::new()));
    let _ = abi.mount(DATA_MOUNT, volume);
    ABI_FS.lock().replace(abi)
}

/// Put back the table [`install_abi_data_for_test`] returned.
#[cfg(lazyos_tests)]
pub fn restore_abi_for_test(previous: Option<Vfs>) {
    *ABI_FS.lock() = previous;
}
