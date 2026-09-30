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

mod attrs;
mod fat_corruption;
mod fat_dirs;
mod fat_image;
mod fat_lfn;
mod fat_soak;
mod ramfs_and_permissions;
mod ramfs_limits;
mod traversal_and_cache;

pub(super) use attrs::*;
pub(super) use fat_corruption::*;
pub(super) use fat_dirs::*;
pub(super) use fat_lfn::*;
pub(super) use fat_soak::*;
pub(super) use ramfs_and_permissions::*;
pub(super) use ramfs_limits::*;
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
        "fs_fat_lfn_single_and_multi_entry",
        fat_lfn_single_and_multi_entry,
    ),
    ("fs_fat_lfn_boundary_lengths", fat_lfn_boundary_lengths),
    (
        "fs_fat_lfn_malformed_runs_fall_back",
        fat_lfn_malformed_runs_fall_back,
    ),
    ("fs_fat_dirs_nested_resolution", fat_dirs_nested_resolution),
    (
        "fs_fat_dirs_fragmented_multi_cluster",
        fat_dirs_fragmented_multi_cluster,
    ),
    (
        "fs_fat_dirs_full_cluster_ends_cleanly",
        fat_dirs_full_cluster_ends_cleanly,
    ),
    (
        "fs_fat_dirs_corrupt_chains_terminate",
        fat_dirs_corrupt_chains_terminate,
    ),
    ("fs_fat_dirs_unique_inodes", fat_dirs_unique_inodes),
    (
        "fs_fat_dirs_overlay_copy_up_nested",
        fat_dirs_overlay_copy_up_nested,
    ),
    ("fs_fat_dirs_on_fat16", fat_dirs_on_fat16),
    ("fs_fat_soak_random_tree", fat_soak_random_tree),
    (
        "fs_ramfs_rename_same_path_and_cycles",
        ramfs_rename_same_path_and_cycles,
    ),
    ("fs_ramfs_byte_cap_enospc", ramfs_byte_cap_enospc),
    ("fs_ramfs_node_cap_enospc", ramfs_node_cap_enospc),
    ("fs_ramfs_soak_fill_and_drain", ramfs_soak_fill_and_drain),
    ("fs_fd_snapshot_shared_and_cow", fd_snapshot_shared_and_cow),
    ("fs_setattr_rule_table", setattr_rule_table),
    (
        "fs_setattr_through_vfs_and_cache",
        setattr_through_vfs_and_cache,
    ),
    ("fs_ramfs_timestamps", ramfs_timestamps),
];
