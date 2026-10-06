//! VFS core (issue #98).

use super::*;
use crate::fs::ramfs::RamFs;
use crate::fs::vfs::{self, FileKind, FsError, Id, Meta, Path, Vfs};
use alloc::sync::Arc;

/// A fresh VFS with ramfs mounted at `/`.
fn ram_vfs() -> Vfs {
    let mut vfs = Vfs::new();
    vfs.mount(
        "/",
        Arc::new(RamFs::new()),
        crate::fs::vfs::MountFlags::default(),
    )
    .expect("mount ramfs at /");
    vfs
}

/// Friendly, debuggable conversion for `?` in tests.
fn fs_error(error: FsError) -> String {
    format!("{} ({error:?})", error.message())
}

mod attrs;
mod fat_corruption;
pub(super) mod fat_image;
mod fat_lfn;
mod ramfs_and_permissions;
mod ramfs_limits;
mod ramfs_quota;
mod traversal_and_cache;

pub(super) use attrs::*;
pub(super) use fat_corruption::*;
pub(super) use fat_lfn::*;
pub(super) use ramfs_and_permissions::*;
pub(super) use ramfs_limits::*;
pub(super) use ramfs_quota::*;
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
    (
        "fs_fat_malformed_geometry_rejected",
        fat_malformed_geometry_rejected,
    ),
    (
        "fs_fat_partition_bounds_checked",
        fat_partition_bounds_checked,
    ),
    (
        "fs_fat_chain_pointers_validated",
        fat_chain_pointers_validated,
    ),
    (
        "fs_fat_chain_length_and_size_bounded",
        fat_chain_length_and_size_bounded,
    ),
    (
        "fs_fat_volume_uses_its_own_device",
        fat_volume_uses_its_own_device,
    ),
    (
        "fs_ramfs_rename_same_path_and_cycles",
        ramfs_rename_same_path_and_cycles,
    ),
    ("fs_ramfs_byte_cap_enospc", ramfs_byte_cap_enospc),
    ("fs_ramfs_node_cap_enospc", ramfs_node_cap_enospc),
    ("fs_ramfs_soak_fill_and_drain", ramfs_soak_fill_and_drain),
    ("fs_ramfs_per_uid_caps", ramfs_per_uid_caps),
    (
        "fs_ramfs_chown_moves_the_charge",
        ramfs_chown_moves_the_charge,
    ),
    ("fs_ramfs_per_uid_soak", ramfs_per_uid_soak),
    ("fs_setattr_rule_table", setattr_rule_table),
    (
        "fs_setattr_through_vfs_and_cache",
        setattr_through_vfs_and_cache,
    ),
    ("fs_ramfs_timestamps", ramfs_timestamps),
    ("fs_fat_lfn_names_assemble", fat_lfn_names_assemble),
    (
        "fs_fat_lfn_inconsistent_runs_fall_back",
        fat_lfn_inconsistent_runs_fall_back,
    ),
    ("fs_fat_subdirectories_resolve", fat_subdirectories_resolve),
    (
        "fs_fat_directory_chain_corruption",
        fat_directory_chain_corruption,
    ),
    ("fs_fat_overlay_copy_up_nested", fat_overlay_copy_up_nested),
    ("fs_fat_tree_soak", fat_tree_soak),
];
