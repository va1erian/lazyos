//! Filesystem: the VFS core, a read-only FAT12/16 volume mounted from the boot
//! disk, and a ramfs scratch mount at `/tmp` (issue #98).
//!
//! The FAT reader and the in-memory ramfs both implement [`vfs::Filesystem`];
//! [`init`] mounts the FAT volume at `/` and ramfs at `/tmp`. The helpers below
//! are the kernel-side entry points (the native loader and the Linux ABI fd
//! layer); they stamp the current task's credentials through [`vfs::Id`], so
//! permission checks apply to every read, not just the Linux syscalls.

mod fat;
pub mod ramfs;
pub mod vfs;

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use vfs::{DirEntry, FsError, Id, Meta, Vfs};

/// The kernel's one VFS: mount table, caches, and whether the boot volume
/// mounted. `None` until [`init`] runs.
static FS: Mutex<Option<(Vfs, bool)>> = Mutex::new(None);

/// Mount the boot volume at `/` and a fresh ramfs at `/tmp`. Returns whether a
/// FAT volume was found (the ramfs mount always succeeds). Idempotent: a second
/// call reports the first call's boot-volume result without remounting.
pub fn init() -> bool {
    let mut global = FS.lock();
    if let Some((_, mounted)) = global.as_ref() {
        return *mounted;
    }
    let mut vfs = Vfs::new();
    let mounted = match fat::Fat16::open() {
        Some(volume) => vfs.mount("/", Arc::new(volume)).is_ok(),
        None => false,
    };
    // `/tmp` is the scratch filesystem: writable, in memory, and discarded on
    // reboot. Mounting it even when FAT is missing keeps the VFS usable.
    let _ = vfs.mount("/tmp", Arc::new(ramfs::RamFs::new()));
    for (point, name) in vfs.mounts() {
        crate::serial_println!("fs: mounted {name} at {point}");
    }
    *global = Some((vfs, mounted));
    mounted
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

/// Metadata for a path: `(size, is_dir)`, as the native loader wants it.
pub fn stat(name: &str) -> Option<(u32, bool)> {
    let id = Id::current();
    with(|vfs| {
        vfs.stat(id, name).ok().map(|meta| {
            let is_dir = meta.kind == vfs::FileKind::Dir;
            (meta.size as u32, is_dir)
        })
    })
    .flatten()
}

/// List the root directory as `(name, is_dir, size)`. Only used by diagnostics
/// today; the Linux layer lists directories through [`vfs_readdir`].
#[cfg_attr(not(laZYOS_TESTS), allow(dead_code))]
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

/// Metadata through the VFS (permission-checked; `__`-free results for the
/// Linux layer to map to errno).
pub fn vfs_stat(id: Id, path: &str) -> Result<Meta, FsError> {
    with(|vfs| vfs.stat(id, path)).unwrap_or(Err(FsError::NotFound))
}

/// Access check on one path (`mask` is a combination of [`vfs::READ`],
/// [`vfs::WRITE`], [`vfs::EXECUTE`]).
pub fn vfs_check(id: Id, path: &str, mask: u8) -> Result<Meta, FsError> {
    with(|vfs| vfs.check(id, path, mask)).unwrap_or(Err(FsError::NotFound))
}

/// Read a whole file through the VFS.
pub fn vfs_read(id: Id, path: &str) -> Result<Vec<u8>, FsError> {
    with(|vfs| vfs.read_file(id, path)).unwrap_or(Err(FsError::NotFound))
}

/// List a directory through the VFS.
pub fn vfs_readdir(id: Id, path: &str) -> Result<Vec<DirEntry>, FsError> {
    with(|vfs| vfs.readdir(id, path)).unwrap_or(Err(FsError::NotFound))
}

/// Write at an offset through the VFS (used by tests and future writers).
#[cfg_attr(not(laZYOS_TESTS), allow(dead_code))]
pub fn vfs_write(id: Id, path: &str, offset: u64, data: &[u8]) -> Result<usize, FsError> {
    with(|vfs| vfs.write(id, path, offset, data)).unwrap_or(Err(FsError::NotFound))
}

/// Create a regular file through the VFS.
#[cfg_attr(not(laZYOS_TESTS), allow(dead_code))]
pub fn vfs_create(id: Id, path: &str, mode: u16) -> Result<Meta, FsError> {
    with(|vfs| vfs.create(id, path, mode)).unwrap_or(Err(FsError::NotFound))
}

/// Create a directory through the VFS.
#[cfg_attr(not(laZYOS_TESTS), allow(dead_code))]
pub fn vfs_mkdir(id: Id, path: &str, mode: u16) -> Result<Meta, FsError> {
    with(|vfs| vfs.mkdir(id, path, mode)).unwrap_or(Err(FsError::NotFound))
}

/// Remove a regular file through the VFS.
#[cfg_attr(not(laZYOS_TESTS), allow(dead_code))]
pub fn vfs_unlink(id: Id, path: &str) -> Result<(), FsError> {
    with(|vfs| vfs.unlink(id, path)).unwrap_or(Err(FsError::NotFound))
}

/// Rename within one mount through the VFS.
#[cfg_attr(not(laZYOS_TESTS), allow(dead_code))]
pub fn vfs_rename(id: Id, from: &str, to: &str) -> Result<(), FsError> {
    with(|vfs| vfs.rename(id, from, to)).unwrap_or(Err(FsError::NotFound))
}

/// The global creation mask.
#[cfg_attr(not(laZYOS_TESTS), allow(dead_code))] // read back by tests/diagnostics
pub fn vfs_umask() -> u16 {
    with(|vfs| vfs.umask()).unwrap_or(0)
}

/// Set the global creation mask, returning the previous one (`umask(2)`).
pub fn vfs_set_umask(mask: u16) -> u16 {
    with(|vfs| vfs.set_umask(mask)).unwrap_or(0)
}
