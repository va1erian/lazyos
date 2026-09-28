//! VFS core (issue #98).

use super::*;
use crate::fs::ramfs::RamFs;
use crate::fs::vfs::{self, FileKind, FsError, Id, Meta, Path, Vfs};
use alloc::sync::Arc;

/// A fresh VFS with ramfs mounted at `/`.
fn ram_vfs() -> Vfs {
    let mut vfs = Vfs::new();
    vfs.mount("/", Arc::new(RamFs::new()))
        .expect("mount ramfs at /");
    vfs
}

/// Friendly, debuggable conversion for `?` in tests.
fn fs_error(error: FsError) -> String {
    format!("{} ({error:?})", error.message())
}

mod ramfs_and_permissions;
mod traversal_and_cache;

pub(super) use ramfs_and_permissions::*;
pub(super) use traversal_and_cache::*;

pub(super) const CASES: &[(&str, Test)] = &[
    ("fs_path_resolution_and_mounts", path_resolution_and_mounts),
    (
        "fs_ramfs_create_write_read_rename_unlink",
        ramfs_create_write_read_rename_unlink,
    ),
    (
        "fs_permission_matrix_owner_group_other",
        permission_matrix_owner_group_other,
    ),
    ("fs_traversal_and_sticky_bits", traversal_and_sticky_bits),
    ("fs_cache_invalidation", cache_invalidation),
    ("fs_fat_read_only_erofs", fat_read_only_erofs),
    ("fs_getdents64_ramfs_directory", getdents64_ramfs_directory),
];
