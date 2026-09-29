//! A read/write ext2 filesystem (issue #99).
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
//! This driver speaks revision 0/1 ext2 with 1 KiB, 2 KiB, or 4 KiB blocks and
//! mounts through the VFS [`Filesystem`] trait, which is the first writable
//! filesystem the platform plan asks for (`docs/platform-plan.md` section 4.4).
//! It implements:
//!
//! * superblock and group descriptors, with the free counters kept in sync;
//! * inode and directory operations: `lookup`, `create`, `mkdir`, `unlink`,
//!   `rename`, `readdir`, and `stat` (through `lookup`);
//! * block allocation from the group bitmaps with per-group accounting;
//! * reads and writes through the direct, single-, double- and triple-indirect
//!   block maps, with sparse holes, and `truncate` (grow and shrink);
//! * timestamps stamped from the PIT (best effort until an RTC driver lands);
//! * a clean/dirty superblock state (`s_state`) and [`Ext2::flush`], which
//!   flushes the device and then marks the volume clean (see `state.rs`).
//!
//! # Deliberate limits
//!
//! * No journal and no guessing: feature bits that change the layout we do not
//!   understand (extents, 64-bit, htree, ...) are rejected in [`Ext2::open`].
//! * Files are capped at [`MAX_FILE_SIZE`] (2 GiB - 1); directories may use
//!   only the direct and single-indirect blocks.
//! * No symlinks or device nodes yet: the VFS only has files and directories,
//!   so an inode with any other type bits answers [`FsError::NotSupported`].
//! * Directories are scanned linearly; an indexed (htree) directory is refused
//!   because a linear scan cannot keep its index coherent.
//! * Only one block-sized buffer is live per helper and every loop is bounded
//!   by a geometry field, so a malformed image cannot hang or panic the kernel.
//!
//! # Locking
//!
//! An instance is `Sync` so several tasks may call it concurrently, but every
//! entry point takes one private mutex: a write updates an inode, a bitmap, and
//! the superblock, and a concurrent lookup must never observe half of that.

use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::min;
use core::sync::atomic::AtomicBool;
use spin::Mutex;

use super::vfs::{DirEntry, FileKind, Filesystem, FsError, Id, Meta, S_IFDIR, S_IFMT, S_IFREG};
use crate::block::{BlockDevice, BlockError, SECTOR_SIZE};

mod blocks;
mod dir;
mod fsimpl;
mod indirect;
mod layout;
mod state;
mod truncate;

use layout::*;

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
    device: &'static dyn BlockDevice,
    /// 1024, 2048, or 4096 bytes.
    block_size: u32,
    /// Device sectors per filesystem block (512-byte sectors today).
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
    /// Whether the on-disk `s_state` currently says clean. Only touched under
    /// `lock`; see `state.rs` for the ordering rules.
    clean: AtomicBool,
    /// Serialises every operation; see the module docs.
    lock: Mutex<()>,
}
impl Ext2 {
    /// Probe `device` for an ext2 superblock and mount it. Any malformed or
    /// unsupported image is refused with a friendly [`FsError`]; nothing here
    /// trusts the disk.
    pub fn open(device: &'static dyn BlockDevice) -> Result<Ext2, FsError> {
        // Every block device in this tree speaks 512-byte sectors. The
        // superblock sits at byte 1024, so reading sectors 2..4 works for any
        // block size; a device with another sector size is refused.
        if device.sector_size() != SECTOR_SIZE {
            return Err(FsError::NotSupported);
        }
        let device_bytes = device
            .sector_count()
            .saturating_mul(device.sector_size() as u64);
        if device_bytes < SUPER_OFFSET + 1024 {
            return Err(FsError::Invalid);
        }
        let mut superblock = [0u8; 1024];
        device
            .read_sectors(SUPER_OFFSET / SECTOR_SIZE as u64, &mut superblock)
            .map_err(io_error)?;
        if le16(&superblock, SB_MAGIC) != EXT2_MAGIC {
            return Err(FsError::Invalid);
        }

        let log_block_size = le32(&superblock, SB_LOG_BLOCK_SIZE);
        if log_block_size > 2 {
            return Err(FsError::Invalid); // 1024 << n, at most 4096
        }
        let block_size = 1024u32 << log_block_size;
        let inodes_count = le32(&superblock, SB_INODES_COUNT);
        let blocks_count = le32(&superblock, SB_BLOCKS_COUNT);
        let free_blocks = le32(&superblock, SB_FREE_BLOCKS);
        let free_inodes = le32(&superblock, SB_FREE_INODES);
        let first_data_block = le32(&superblock, SB_FIRST_DATA_BLOCK);
        let blocks_per_group = le32(&superblock, SB_BLOCKS_PER_GROUP);
        let inodes_per_group = le32(&superblock, SB_INODES_PER_GROUP);
        let rev_level = le32(&superblock, SB_REV_LEVEL);
        let inode_size = if rev_level >= 1 {
            le16(&superblock, SB_INODE_SIZE)
        } else {
            INODE_CORE_SIZE as u16
        };
        let first_ino = if rev_level >= 1 {
            le32(&superblock, SB_FIRST_INO)
        } else {
            DEFAULT_FIRST_INO
        };
        let feature_incompat = le32(&superblock, SB_FEATURE_INCOMPAT);
        let feature_ro = le32(&superblock, SB_FEATURE_RO_COMPAT);

        if inodes_count < ROOT_INO || blocks_count <= first_data_block {
            return Err(FsError::Invalid);
        }
        if free_blocks > blocks_count || free_inodes > inodes_count {
            return Err(FsError::Invalid);
        }
        if first_data_block > 1 {
            return Err(FsError::Invalid); // 1 for 1K blocks, 0 otherwise
        }
        if blocks_per_group == 0 || inodes_per_group == 0 {
            return Err(FsError::Invalid);
        }
        if inode_size < INODE_CORE_SIZE as u16
            || u32::from(inode_size) > block_size
            || !block_size.is_multiple_of(u32::from(inode_size))
        {
            return Err(FsError::Invalid);
        }
        if first_ino == 0 || first_ino > inodes_count {
            return Err(FsError::Invalid);
        }
        // The descriptor table has one 32-byte entry per group; the group count
        // comes from the block count, and the inodes may not span more groups.
        let block_span = blocks_count - first_data_block;
        let groups = block_span.div_ceil(blocks_per_group);
        let inode_groups = inodes_count.div_ceil(inodes_per_group);
        if groups == 0 || groups > MAX_GROUPS || inode_groups > groups {
            return Err(FsError::Invalid);
        }
        let gdt_block = u64::from(first_data_block) + 1;
        let gdt_blocks = (u64::from(groups) * GD_SIZE as u64).div_ceil(u64::from(block_size));
        if gdt_block + gdt_blocks > u64::from(blocks_count) {
            return Err(FsError::Invalid);
        }
        if u64::from(blocks_count)
            .checked_mul(u64::from(block_size))
            .is_none_or(|bytes| bytes > device_bytes)
        {
            return Err(FsError::Invalid);
        }
        if feature_incompat & !FEATURE_INCOMPAT_FILETYPE != 0 {
            return Err(FsError::NotSupported);
        }
        if feature_ro & !(FEATURE_RO_SPARSE_SUPER | FEATURE_RO_LARGE_FILE) != 0 {
            return Err(FsError::NotSupported);
        }
        let inodes_per_block = block_size / u32::from(inode_size);
        if !inodes_per_group.is_multiple_of(inodes_per_block) {
            return Err(FsError::Invalid);
        }
        let mount_state = le16(&superblock, SB_STATE);
        let read_only = !device.is_writable();
        Self::report_mount_state(device.name(), mount_state);

        Ok(Ext2 {
            device,
            block_size,
            sectors_per_block: block_size / SECTOR_SIZE as u32,
            inodes_count,
            blocks_count,
            first_data_block,
            blocks_per_group,
            inodes_per_group,
            inode_size,
            inodes_per_block,
            ptrs_per_block: block_size / 4,
            first_ino,
            groups,
            gdt_block,
            has_file_type: feature_incompat & FEATURE_INCOMPAT_FILETYPE != 0,
            has_large_file: feature_ro & FEATURE_RO_LARGE_FILE != 0,
            read_only,
            mount_state,
            clean: AtomicBool::new(!read_only && mount_state & STATE_VALID != 0),
            lock: Mutex::new(()),
        })
    }

    /// Bytes per filesystem block.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // diagnostics/tests
    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    /// The superblock's free-block counter (the future `statfs` surface).
    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn free_blocks(&self) -> Result<u32, FsError> {
        let _guard = self.lock.lock();
        let mut raw = [0u8; 1024];
        self.read_super_raw(&mut raw)?;
        Ok(le32(&raw, SB_FREE_BLOCKS))
    }

    /// The superblock's free-inode counter.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn free_inodes(&self) -> Result<u32, FsError> {
        let _guard = self.lock.lock();
        let mut raw = [0u8; 1024];
        self.read_super_raw(&mut raw)?;
        Ok(le32(&raw, SB_FREE_INODES))
    }

    /// The on-disk link count of `path`'s inode; the diagnostic the tests use
    /// to check directory bookkeeping (`.`/`..` links) after renames.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn link_count(&self, path: &str) -> Result<u16, FsError> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        Ok(le16(&self.read_inode(ino)?, INO_LINKS))
    }

    /// The physical block backing logical `index` of `path` (`0` for a hole).
    /// This is the diagnostic surface the tests use to see allocation reuse.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn mapped_block(&self, path: &str, index: u32) -> Result<u32, FsError> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        let inode = self.read_inode(ino)?;
        if kind_from_mode(le16(&inode, INO_MODE)) != Some(FileKind::File) {
            return Err(FsError::IsDir);
        }
        self.block_map(&inode, index)
    }

    /// Make everything written so far durable and mark the volume clean.
    /// This is the umount/fsync/shutdown surface (`state.rs` has the ordering).
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // the trait method is the caller
    pub fn flush(&self) -> Result<(), FsError> {
        self.sync_volume()
    }

    /// Read one filesystem block into `buf` (at least `block_size` bytes).
    fn read_block(&self, block: u64, buf: &mut [u8]) -> Result<(), FsError> {
        let size = self.block_size as usize;
        if block >= u64::from(self.blocks_count) || buf.len() < size {
            return Err(FsError::Invalid);
        }
        self.device
            .read_sectors(block * u64::from(self.sectors_per_block), &mut buf[..size])
            .map_err(io_error)
    }

    /// Zero one filesystem block on disk.
    ///
    /// Every freshly allocated block is zeroed here before its pointer is
    /// linked anywhere (the inode, or an indirect block already committed to
    /// disk). Linking a pointer to an allocated-but-unwritten block would let
    /// the block's previous owner's bytes (or, on a device that reads back
    /// garbage, uninitialized bytes) surface where a hole should read as
    /// zero -- a cross-file, potentially cross-user information leak
    /// (CWE-200) if the write of the caller's actual data then fails.
    fn zero_block(&self, block: u64) -> Result<(), FsError> {
        let size = self.block_size as usize;
        let zeroed = [0u8; MAX_BLOCK_SIZE];
        self.write_block(block, &zeroed[..size])
    }

    /// Write one filesystem block from `buf`.
    fn write_block(&self, block: u64, buf: &[u8]) -> Result<(), FsError> {
        let size = self.block_size as usize;
        if block >= u64::from(self.blocks_count) || buf.len() < size {
            return Err(FsError::Invalid);
        }
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        // Before the first change lands, the volume must already say "dirty".
        self.mark_dirty()?;
        self.device
            .write_sectors(block * u64::from(self.sectors_per_block), &buf[..size])
            .map_err(io_error)
    }

    /// Read the 1024-byte superblock, wherever its block starts.
    fn read_super_raw(&self, raw: &mut [u8; 1024]) -> Result<(), FsError> {
        let size = self.block_size as usize;
        let mut block = [0u8; MAX_BLOCK_SIZE];
        let sb_block = SUPER_OFFSET / u64::from(self.block_size);
        self.read_block(sb_block, &mut block[..size])?;
        let start = (SUPER_OFFSET % u64::from(self.block_size)) as usize;
        raw.copy_from_slice(&block[start..start + 1024]);
        Ok(())
    }

    /// Patch the superblock in place (read-modify-write keeps every other byte).
    fn write_super_raw(&self, raw: &[u8; 1024]) -> Result<(), FsError> {
        let size = self.block_size as usize;
        let mut block = [0u8; MAX_BLOCK_SIZE];
        let sb_block = SUPER_OFFSET / u64::from(self.block_size);
        self.read_block(sb_block, &mut block[..size])?;
        let start = (SUPER_OFFSET % u64::from(self.block_size)) as usize;
        block[start..start + 1024].copy_from_slice(raw);
        self.write_block(sb_block, &block[..size])
    }

    /// Add a delta to the superblock's free counters, refusing to cross zero
    /// or exceed the totals (which would mean the image is inconsistent).
    fn update_counts(&self, blocks_delta: i32, inodes_delta: i32) -> Result<(), FsError> {
        let mut raw = [0u8; 1024];
        self.read_super_raw(&mut raw)?;
        let blocks = i64::from(le32(&raw, SB_BLOCKS_COUNT));
        let inodes = i64::from(le32(&raw, SB_INODES_COUNT));
        let free_blocks = i64::from(le32(&raw, SB_FREE_BLOCKS)) + i64::from(blocks_delta);
        let free_inodes = i64::from(le32(&raw, SB_FREE_INODES)) + i64::from(inodes_delta);
        if free_blocks < 0 || free_blocks > blocks || free_inodes < 0 || free_inodes > inodes {
            return Err(FsError::Invalid);
        }
        put32(&mut raw, SB_FREE_BLOCKS, free_blocks as u32);
        put32(&mut raw, SB_FREE_INODES, free_inodes as u32);
        put32(&mut raw, SB_WTIME, now());
        self.write_super_raw(&raw)
    }

    /// Read one group descriptor, validating its block pointers.
    fn read_group(&self, group: u32) -> Result<GroupDesc, FsError> {
        if group >= self.groups {
            return Err(FsError::Invalid);
        }
        let byte = self.gdt_block * u64::from(self.block_size) + u64::from(group) * GD_SIZE as u64;
        let block = byte / u64::from(self.block_size);
        let offset = (byte % u64::from(self.block_size)) as usize;
        let size = self.block_size as usize;
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        self.read_block(block, &mut buf[..size])?;
        let desc = GroupDesc {
            block_bitmap: le32(&buf, offset + GD_BLOCK_BITMAP),
            inode_bitmap: le32(&buf, offset + GD_INODE_BITMAP),
            inode_table: le32(&buf, offset + GD_INODE_TABLE),
            free_blocks: le16(&buf, offset + GD_FREE_BLOCKS),
            free_inodes: le16(&buf, offset + GD_FREE_INODES),
            used_dirs: le16(&buf, offset + GD_USED_DIRS),
        };
        if desc.block_bitmap >= self.blocks_count
            || desc.inode_bitmap >= self.blocks_count
            || desc.inode_table >= self.blocks_count
        {
            return Err(FsError::Invalid);
        }
        Ok(desc)
    }

    /// Patch one group descriptor in place.
    fn write_group(&self, group: u32, desc: &GroupDesc) -> Result<(), FsError> {
        let byte = self.gdt_block * u64::from(self.block_size) + u64::from(group) * GD_SIZE as u64;
        let block = byte / u64::from(self.block_size);
        let offset = (byte % u64::from(self.block_size)) as usize;
        let size = self.block_size as usize;
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        self.read_block(block, &mut buf[..size])?;
        put32(&mut buf, offset + GD_BLOCK_BITMAP, desc.block_bitmap);
        put32(&mut buf, offset + GD_INODE_BITMAP, desc.inode_bitmap);
        put32(&mut buf, offset + GD_INODE_TABLE, desc.inode_table);
        put16(&mut buf, offset + GD_FREE_BLOCKS, desc.free_blocks);
        put16(&mut buf, offset + GD_FREE_INODES, desc.free_inodes);
        put16(&mut buf, offset + GD_USED_DIRS, desc.used_dirs);
        self.write_block(block, &buf[..size])
    }

    /// Whether bit `index` of a bitmap is set.
    fn bitmap_test(buf: &[u8], index: u32) -> bool {
        buf[(index / 8) as usize] & (1 << (index % 8)) != 0
    }

    fn bitmap_set(buf: &mut [u8], index: u32) {
        buf[(index / 8) as usize] |= 1 << (index % 8);
    }

    fn bitmap_clear(buf: &mut [u8], index: u32) {
        buf[(index / 8) as usize] &= !(1 << (index % 8));
    }

    /// The first clear bit in `start..bits`, scanning in allocation order.
    fn bitmap_find_zero(buf: &[u8], start: u32, bits: u32) -> Option<u32> {
        (start..bits).find(|&index| !Self::bitmap_test(buf, index))
    }
}
