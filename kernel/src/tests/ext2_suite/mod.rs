//! ext2 read/write filesystem (issue #99).

use super::block_suite::FakeDisk;
use super::*;
use crate::block::{self, SECTOR_SIZE};
use crate::fs::ext2::Ext2;
use crate::fs::vfs::{self, FileKind, FsError, Id, Vfs};
use alloc::sync::Arc;
use core::sync::atomic::Ordering;

/// Sectors in every test disk (512 KiB at 512 bytes per sector).
const DISK_SECTORS: usize = 1024;

/// Byte offset of the ext2 superblock (fixed by the format).
const SUPER: usize = 1024;

fn put16(image: &mut [u8], offset: usize, value: u16) {
    image[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(image: &mut [u8], offset: usize, value: u32) {
    image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Friendly, debuggable conversion for `?` in tests.
fn fs_error(error: FsError) -> String {
    format!("{} ({error:?})", error.message())
}

/// A miniature `mke2fs` for the tests: one block group, 64 inodes, and a
/// root directory holding only `.` and `..`. It lays out exactly the
/// structures `Ext2::open` validates, so the suite exercises the real
/// on-disk format with no disk image and no userspace tool. Returns the
/// raw image for `total_blocks` blocks of the requested size.
fn mkfs(block_size: u32, total_blocks: u32, inode_count: u32) -> Vec<u8> {
    let mut image = vec![0u8; DISK_SECTORS * SECTOR_SIZE];
    let bs = block_size as usize;
    let inode_size = 128usize;
    // Blocks 0..first_data hold the boot block; the superblock is at byte
    // 1024, i.e. block 1 for 1K blocks and block 0 for 2K/4K blocks.
    let first_data = if block_size == 1024 { 1 } else { 0 };
    let gdt = first_data + 1;
    let block_bitmap = gdt + 1;
    let inode_bitmap = gdt + 2;
    let inode_table = gdt + 3;
    let table_blocks = (inode_count as usize * inode_size).div_ceil(bs) as u32;
    let root_block = inode_table + table_blocks;
    let used_end = root_block + 1;
    let free_blocks = total_blocks - used_end;
    // Inodes 1..10 are reserved (the root is inode 2 among them); the
    // free count must not count them.
    let free_inodes = inode_count - 10;

    // Superblock.
    put32(&mut image, SUPER + 0x00, inode_count);
    put32(&mut image, SUPER + 0x04, total_blocks);
    put32(&mut image, SUPER + 0x0C, free_blocks);
    put32(&mut image, SUPER + 0x10, free_inodes);
    put32(&mut image, SUPER + 0x14, first_data);
    put32(&mut image, SUPER + 0x18, block_size.trailing_zeros() - 10);
    put32(&mut image, SUPER + 0x1C, block_size.trailing_zeros() - 10);
    put32(&mut image, SUPER + 0x20, block_size * 8);
    put32(&mut image, SUPER + 0x24, block_size * 8);
    put32(&mut image, SUPER + 0x28, inode_count);
    put16(&mut image, SUPER + 0x38, 0xEF53);
    put16(&mut image, SUPER + 0x3A, 1); // clean
    put16(&mut image, SUPER + 0x3C, 1); // continue on errors
    put32(&mut image, SUPER + 0x4C, 1); // revision 1
    put32(&mut image, SUPER + 0x54, 11); // first non-reserved inode
    put16(&mut image, SUPER + 0x58, inode_size as u16);
    put32(&mut image, SUPER + 0x60, 0x2); // incompat: filetype
    image[SUPER + 0x78..SUPER + 0x88].copy_from_slice(b"lazyos-ext2\0\0\0\0\0");

    // Group descriptor 0.
    let gd = gdt as usize * bs;
    put32(&mut image, gd + 0x00, block_bitmap);
    put32(&mut image, gd + 0x04, inode_bitmap);
    put32(&mut image, gd + 0x08, inode_table);
    put16(&mut image, gd + 0x0C, free_blocks as u16);
    put16(&mut image, gd + 0x0E, free_inodes as u16);
    put16(&mut image, gd + 0x10, 1); // the root is one directory

    // Block bitmap: every metadata block plus the root block is used; the
    // bits past the volume are padding (set, like mke2fs).
    let bitmap = block_bitmap as usize * bs;
    let bitmap_bits = block_size * 8; // blocks per group
    for bit in 0..bitmap_bits {
        let block = first_data + bit;
        if block < used_end || block >= total_blocks {
            image[bitmap + (bit / 8) as usize] |= 1 << (bit % 8);
        }
    }
    // Inode bitmap: the reserved inodes 1..10 (including the root) are
    // used, and the bits past `inode_count` are padding.
    let ib = inode_bitmap as usize * bs;
    for ino in 1..11.min(inode_count + 1) {
        image[ib + ((ino - 1) / 8) as usize] |= 1 << ((ino - 1) % 8);
    }
    for bit in inode_count..(block_size * 8) {
        image[ib + (bit / 8) as usize] |= 1 << (bit % 8);
    }

    // Root inode (number 2). `i_dtime` is a full 32-bit field, so gid,
    // links, and i_blocks start at 0x18, 0x1A, and 0x1C.
    let root_inode = inode_table as usize * bs + inode_size;
    put16(&mut image, root_inode + 0x00, 0o040755);
    put32(&mut image, root_inode + 0x04, block_size); // size
    put32(&mut image, root_inode + 0x08, 1); // atime
    put32(&mut image, root_inode + 0x0C, 1); // ctime
    put32(&mut image, root_inode + 0x10, 1); // mtime
    put16(&mut image, root_inode + 0x1A, 2); // links
    put32(&mut image, root_inode + 0x1C, block_size / 512); // i_blocks
    put32(&mut image, root_inode + 0x28, root_block);

    // Root directory block: `.` then `..` filling the block.
    let root = root_block as usize * bs;
    put32(&mut image, root, 2);
    put16(&mut image, root + 4, 12);
    image[root + 6] = 1;
    image[root + 7] = 2;
    image[root + 8] = b'.';
    put32(&mut image, root + 12, 2);
    put16(&mut image, root + 16, (bs - 12) as u16);
    image[root + 18] = 2;
    image[root + 19] = 2;
    image[root + 20] = b'.';
    image[root + 21] = b'.';

    image
}

/// Format a fresh fake disk, open it, and mount it as a private VFS root.
/// The disk comes back too, so tests can watch its write/flush counters.
pub(super) fn mounted(
    block_size: u32,
    total_blocks: u32,
) -> Result<(Arc<Ext2>, Vfs, &'static FakeDisk), String> {
    mounted_in(0, block_size, total_blocks)
}

/// The suite's reusable test disks. The block layer wants `'static` devices,
/// so a disk can only be leaked, and the kernel heap is 16 MiB: reformatting
/// one of a few pooled 512 KiB disks per test keeps the whole suite (and its
/// soaks) from leaking a disk per call.
fn pooled_disk(slot: usize) -> &'static FakeDisk {
    const NAMES: [&str; 3] = ["test-ext2", "test-ext2-b", "test-ext2-c"];
    static POOL: spin::Mutex<[Option<&'static FakeDisk>; 3]> = spin::Mutex::new([None; 3]);
    let disk = *POOL.lock()[slot].get_or_insert_with(|| FakeDisk::new(NAMES[slot], DISK_SECTORS));
    disk.fail_nth_write(u32::MAX); // a failed test must not arm the next one
    disk
}

/// [`mounted`] on pooled disk `slot`, for tests that need several volumes.
pub(super) fn mounted_in(
    slot: usize,
    block_size: u32,
    total_blocks: u32,
) -> Result<(Arc<Ext2>, Vfs, &'static FakeDisk), String> {
    let image = mkfs(block_size, total_blocks, 64);
    let disk = pooled_disk(slot);
    disk.data.lock().copy_from_slice(&image);
    let fs = Arc::new(Ext2::open(disk).map_err(fs_error)?);
    let mut vfs = Vfs::new();
    vfs.mount("/", fs.clone()).map_err(fs_error)?;
    Ok((fs, vfs, disk))
}

pub(super) mod data_fds;
mod fixtures;
mod format_and_roundtrip;
mod integrity;
mod large_files;
mod orphan_crash;
mod orphans;
mod persistence;
mod sync_state;
mod truncate;

use fixtures::*;
pub(super) use format_and_roundtrip::*;
pub(super) use integrity::*;
pub(super) use large_files::*;
pub(super) use orphan_crash::*;
pub(super) use orphans::*;
pub(super) use persistence::*;
pub(super) use sync_state::*;
pub(super) use truncate::*;

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "fs_ext2_create_write_read_rename_unlink",
        create_write_read_rename_unlink,
    ),
    ("fs_ext2_block_sizes", block_sizes),
    (
        "fs_ext2_rename_over_existing_replaces",
        rename_over_existing_replaces,
    ),
    (
        "fs_ext2_rename_dir_over_empty_dir_frees_victim",
        rename_dir_over_empty_dir_frees_victim,
    ),
    (
        "fs_ext2_soak_rename_dir_over_empty_dir",
        soak_rename_dir_over_empty_dir,
    ),
    ("fs_ext2_rejects_corruption", rejects_corruption),
    ("fs_ext2_mount_device_wiring", mount_device_wiring),
    ("fs_ext2_files_survive_remount", files_survive_remount),
    ("fs_ext2_soak_remount_generations", soak_remount_generations),
    (
        "fs_ext2_truncate_shrink_grow_zero",
        truncate_shrink_grow_zero,
    ),
    ("fs_ext2_truncate_across_indirect", truncate_across_indirect),
    ("fs_ext2_truncate_sparse_files", truncate_sparse_files),
    ("fs_ext2_truncate_bad_inputs", truncate_bad_inputs),
    (
        "fs_ext2_truncate_survives_remount",
        truncate_survives_remount,
    ),
    ("fs_ext2_truncate_crash_sweep", truncate_crash_sweep),
    (
        "fs_ext2_double_indirect_boundaries",
        double_indirect_boundaries,
    ),
    (
        "fs_ext2_double_indirect_contiguous_file",
        double_indirect_contiguous_file,
    ),
    ("fs_ext2_size_cap_is_enforced", size_cap_is_enforced),
    (
        "fs_ext2_soak_write_truncate_unlink",
        soak_write_truncate_unlink,
    ),
    ("fs_ext2_soak_fill_and_free_large", soak_fill_and_free_large),
    ("fs_ext2_state_dirty_then_clean", state_dirty_then_clean),
    (
        "fs_ext2_state_unclean_mount_is_not_laundered",
        state_unclean_mount_is_not_laundered,
    ),
    (
        "fs_ext2_state_marker_write_failures",
        state_marker_write_failures,
    ),
    (
        "fs_ext2_sync_all_flushes_every_mount",
        sync_all_flushes_every_mount,
    ),
    ("fs_ext2_data_volume_probe", data_volume_probe),
    ("fs_root_prefers_fat_over_ext2", root_prefers_fat_over_ext2),
    ("fs_ext2_soak_state_generations", soak_state_generations),
    (
        "fs_ext2_orphans_reclaimed_on_unclean_mount",
        orphans_reclaimed_on_unclean_mount,
    ),
    (
        "fs_ext2_clean_volume_is_not_scanned",
        clean_volume_is_not_scanned,
    ),
    (
        "fs_ext2_orphan_lookalikes_are_left_alone",
        lookalikes_are_left_alone,
    ),
    (
        "fs_ext2_data_mount_reclaims_before_exposure",
        data_mount_reclaims_before_exposure,
    ),
    (
        "fs_ext2_orphan_delete_crash_sweep",
        orphan_delete_crash_sweep,
    ),
    (
        "fs_ext2_orphan_reclaim_crash_sweep",
        orphan_reclaim_crash_sweep,
    ),
    ("fs_ext2_soak_orphan_generations", soak_orphan_generations),
];
