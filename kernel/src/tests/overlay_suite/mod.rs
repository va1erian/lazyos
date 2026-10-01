//! Linux ABI copy-up overlay root (issue #136).

use super::*;
use crate::fs::overlay::Overlay;
use crate::fs::ramfs::RamFs;
use crate::fs::vfs::{self, FileKind, Filesystem, FsError, Id, Vfs};
use alloc::sync::Arc;

/// Friendly, debuggable conversion for `?` in tests.
fn fs_error(error: FsError) -> String {
    format!("{} ({error:?})", error.message())
}

/// Lower-layer fixture standing in for the read-only FAT volume: the
/// overlay never calls a mutating method on it, so a ramfs works and lets
/// the tests prove the lower bytes stay untouched.
fn lower_fixture() -> Result<Arc<RamFs>, String> {
    let lower = Arc::new(RamFs::new());
    for (name, data) in [
        ("HELLO.TXT", b"Hello from LazyOS".as_slice()),
        ("LOWER.TXT", b"lower".as_slice()),
    ] {
        lower.create(name, 0o555, Id::ROOT).map_err(fs_error)?;
        lower.write(name, 0, data).map_err(fs_error)?;
    }
    lower.mkdir("LDIR", 0o555, Id::ROOT).map_err(fs_error)?;
    lower
        .create("LDIR/INNER.TXT", 0o555, Id::ROOT)
        .map_err(fs_error)?;
    lower
        .write("LDIR/INNER.TXT", 0, b"inner")
        .map_err(fs_error)?;
    Ok(lower)
}

/// An overlay-mounted VFS plus the handle to inspect its usage.
fn mounted_overlay(lower: Arc<RamFs>) -> Result<(Vfs, Arc<Overlay>), String> {
    let overlay = Arc::new(Overlay::new(lower));
    let mut vfs = Vfs::new();
    vfs.mount("/", overlay.clone(), crate::fs::vfs::MountFlags::default())
        .map_err(fs_error)?;
    Ok((vfs, overlay))
}

/// Read a whole lower file directly, bypassing the overlay.
fn lower_bytes(lower: &RamFs, path: &str) -> Result<Vec<u8>, String> {
    let meta = lower.lookup(path).map_err(fs_error)?;
    let mut data = alloc::vec![0u8; meta.size as usize];
    let read = lower.read(path, 0, &mut data).map_err(fs_error)?;
    data.truncate(read);
    Ok(data)
}

fn names(entries: &[vfs::DirEntry]) -> Vec<String> {
    entries.iter().map(|entry| entry.name.clone()).collect()
}

fn has(entries: &[vfs::DirEntry], name: &str) -> bool {
    entries.iter().any(|entry| entry.name == name)
}

mod attrs;
mod copy_up;
mod lifecycle;

pub(super) use attrs::*;
pub(super) use copy_up::*;
pub(super) use lifecycle::*;

pub(super) const CASES: &[(&str, Test)] = &[
    ("fs_overlay_copy_up_read_write", copy_up_read_write),
    ("fs_overlay_dir_create_remove", dir_create_remove),
    ("fs_overlay_rename_replace", rename_replace),
    ("fs_overlay_enospc_limits", enospc_limits),
    ("fs_overlay_soak_generations", soak_generations),
    ("fs_abi_mkdir_rename_rmdir", abi_syscalls),
    ("fs_abi_unlink_while_open", unlink_while_open),
    ("fs_overlay_setattr_copies_up", setattr_copies_up),
];
