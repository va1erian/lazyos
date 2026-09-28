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
//! * reads and writes through the direct and single-indirect block maps;
//! * timestamps stamped from the PIT (best effort until an RTC driver lands);
//! * [`Ext2::flush`], which stamps the superblock and flushes the device.
//!
//! # Deliberate limits
//!
//! * No journal and no guessing: feature bits that change the layout we do not
//!   understand (extents, 64-bit, htree, ...) are rejected in [`Ext2::open`].
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
use spin::Mutex;

use super::vfs::{DirEntry, FileKind, Filesystem, FsError, Id, Meta, S_IFDIR, S_IFMT, S_IFREG};
use crate::block::{BlockDevice, BlockError, SECTOR_SIZE};

/// Superblock magic (`s_magic`) at byte 1024 of the volume.
const EXT2_MAGIC: u16 = 0xEF53;
/// The superblock always starts at byte 1024 (block 0 for 2K/4K blocks, the
/// second block for 1K blocks).
const SUPER_OFFSET: u64 = 1024;
/// Root directory inode, fixed by the format.
const ROOT_INO: u32 = 2;
/// Inodes below `first_ino` are reserved (bad-blocks inode, root, ...).
const DEFAULT_FIRST_INO: u32 = 11;
/// Twelve direct block slots; slot 12 is the single indirect.
const DIRECT_BLOCKS: u32 = 12;
const SINGLE_INDIRECT_SLOT: u32 = 12;
const DOUBLE_INDIRECT_SLOT: u32 = 13;
const TRIPLE_INDIRECT_SLOT: u32 = 14;
/// The revision-1 core inode is 128 bytes; larger inode tails are preserved by
/// the read-modify-write in [`Ext2::write_inode`].
const INODE_CORE_SIZE: usize = 128;
/// The largest block this driver handles; 4 KiB keeps stack buffers small.
const MAX_BLOCK_SIZE: usize = 4096;
/// Bound on the block-group count so no per-group loop can run away.
const MAX_GROUPS: u32 = 4096;
/// Directory entry header: inode, record length, name length, file type.
const DE_HEADER: usize = 8;
const DE_INO: usize = 0;
const DE_REC_LEN: usize = 4;
const DE_NAME_LEN: usize = 6;
const DE_FILE_TYPE: usize = 7;
/// File-type byte values in a directory entry.
const FT_REGULAR: u8 = 1;
const FT_DIRECTORY: u8 = 2;
/// The longest ext2 name (and the VFS's own limit).
const MAX_NAME: usize = 255;
/// Incompat feature: directory entries carry a file-type byte.
const FEATURE_INCOMPAT_FILETYPE: u32 = 0x0002;
/// Read-only-compat features this driver understands: sparse superblocks (we
/// do not need the backups) and large files (the size high bits).
const FEATURE_RO_SPARSE_SUPER: u32 = 0x0001;
const FEATURE_RO_LARGE_FILE: u32 = 0x0002;

// Superblock byte offsets within the 1024-byte superblock.
const SB_INODES_COUNT: usize = 0x00;
const SB_BLOCKS_COUNT: usize = 0x04;
const SB_FREE_BLOCKS: usize = 0x0C;
const SB_FREE_INODES: usize = 0x10;
const SB_FIRST_DATA_BLOCK: usize = 0x14;
const SB_LOG_BLOCK_SIZE: usize = 0x18;
const SB_BLOCKS_PER_GROUP: usize = 0x20;
const SB_INODES_PER_GROUP: usize = 0x28;
const SB_WTIME: usize = 0x30;
const SB_MAGIC: usize = 0x38;
const SB_REV_LEVEL: usize = 0x4C;
const SB_FIRST_INO: usize = 0x54;
const SB_INODE_SIZE: usize = 0x58;
const SB_FEATURE_INCOMPAT: usize = 0x60;
const SB_FEATURE_RO_COMPAT: usize = 0x64;

// Inode byte offsets (the first 128 bytes of every inode). `i_dtime` is a
// full 32-bit field, so `i_gid` starts at 0x18, not 0x16.
const INO_MODE: usize = 0x00;
const INO_UID: usize = 0x02;
const INO_SIZE: usize = 0x04;
const INO_ATIME: usize = 0x08;
const INO_CTIME: usize = 0x0C;
const INO_MTIME: usize = 0x10;
const INO_DTIME: usize = 0x14;
const INO_GID: usize = 0x18;
const INO_LINKS: usize = 0x1A;
const INO_BLOCKS: usize = 0x1C;
const INO_BLOCK: usize = 0x28;
const INO_DIR_ACL: usize = 0x6C;

// Group descriptor byte offsets (32 bytes each in the descriptor table).
const GD_BLOCK_BITMAP: usize = 0x00;
const GD_INODE_BITMAP: usize = 0x04;
const GD_INODE_TABLE: usize = 0x08;
const GD_FREE_BLOCKS: usize = 0x0C;
const GD_FREE_INODES: usize = 0x0E;
const GD_USED_DIRS: usize = 0x10;
const GD_SIZE: usize = 32;

fn le16(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

fn le32(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

fn put16(buf: &mut [u8], offset: usize, value: u16) {
    buf[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Map a block-layer failure onto the VFS error space. A write to a read-only
/// device keeps its friendly `EROFS`; everything else is a corrupt or missing
/// backing store.
fn io_error(error: BlockError) -> FsError {
    match error {
        BlockError::ReadOnly => FsError::ReadOnly,
        _ => FsError::Invalid,
    }
}

/// The node kind an ext2 `i_mode` denotes, or `None` for node types the VFS
/// cannot represent yet (symlinks, device nodes, sockets, FIFOs).
fn kind_from_mode(mode: u16) -> Option<FileKind> {
    match mode & S_IFMT {
        S_IFREG => Some(FileKind::File),
        S_IFDIR => Some(FileKind::Dir),
        _ => None,
    }
}

/// The PIT tick counter is 100 Hz (`arch::pic` programs that rate), so uptime
/// in seconds is the best clock until an RTC driver lands. Fresh timestamps
/// therefore restart at zero on every boot; epoch time is a follow-up.
fn now() -> u32 {
    (crate::task::ticks() / 100) as u32
}

/// Refuse an owner whose ids do not fit the 16-bit `i_uid`/`i_gid` fields.
/// Truncating would hand a file created by uid 65536 to uid 0 (root).
fn check_owner(owner: Id) -> Result<(), FsError> {
    if owner.uid > u32::from(u16::MAX) || owner.gid > u32::from(u16::MAX) {
        return Err(FsError::Invalid);
    }
    Ok(())
}

/// Stamp a change: both `ctime` and `mtime` move on content or tree changes.
fn touch(inode: &mut [u8; INODE_CORE_SIZE], time: u32) {
    put32(inode, INO_CTIME, time);
    put32(inode, INO_MTIME, time);
}

/// Split `path` into its parent directory path and final component. The root
/// has no parent, so creating or removing it is [`FsError::Exists`].
fn split_parent(path: &str) -> Result<(&str, &str), FsError> {
    let path = path.trim_matches('/');
    if path.is_empty() {
        return Err(FsError::Exists);
    }
    let (parent, name) = match path.rsplit_once('/') {
        Some((parent, name)) => (parent, name),
        None => ("", path),
    };
    if name.is_empty() || name == "." || name == ".." {
        return Err(FsError::Invalid);
    }
    if name.len() > MAX_NAME {
        return Err(FsError::NameTooLong);
    }
    Ok((parent, name))
}

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
            || block_size % u32::from(inode_size) != 0
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
        if inodes_per_group % inodes_per_block != 0 {
            return Err(FsError::Invalid);
        }

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
            read_only: !device.is_writable(),
            lock: Mutex::new(()),
        })
    }

    /// Bytes per filesystem block.
    #[cfg_attr(not(laZYOS_TESTS), allow(dead_code))] // diagnostics/tests
    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    /// The superblock's free-block counter (the future `statfs` surface).
    #[cfg_attr(not(laZYOS_TESTS), allow(dead_code))]
    pub fn free_blocks(&self) -> Result<u32, FsError> {
        let _guard = self.lock.lock();
        let mut raw = [0u8; 1024];
        self.read_super_raw(&mut raw)?;
        Ok(le32(&raw, SB_FREE_BLOCKS))
    }

    /// The superblock's free-inode counter.
    #[cfg_attr(not(laZYOS_TESTS), allow(dead_code))]
    pub fn free_inodes(&self) -> Result<u32, FsError> {
        let _guard = self.lock.lock();
        let mut raw = [0u8; 1024];
        self.read_super_raw(&mut raw)?;
        Ok(le32(&raw, SB_FREE_INODES))
    }

    /// The physical block backing logical `index` of `path` (`0` for a hole).
    /// This is the diagnostic surface the tests use to see allocation reuse.
    #[cfg_attr(not(laZYOS_TESTS), allow(dead_code))]
    pub fn mapped_block(&self, path: &str, index: u32) -> Result<u32, FsError> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        let inode = self.read_inode(ino)?;
        if kind_from_mode(le16(&inode, INO_MODE)) != Some(FileKind::File) {
            return Err(FsError::IsDir);
        }
        self.block_map(&inode, index)
    }

    /// Stamp `s_wtime` and hand the write cache to the device. ext2 keeps no
    /// journal, so this is the whole durability story for now.
    #[cfg_attr(not(laZYOS_TESTS), allow(dead_code))] // the future umount surface
    pub fn flush(&self) -> Result<(), FsError> {
        let _guard = self.lock.lock();
        if !self.read_only {
            let mut raw = [0u8; 1024];
            self.read_super_raw(&mut raw)?;
            put32(&mut raw, SB_WTIME, now());
            self.write_super_raw(&raw)?;
        }
        self.device.flush().map_err(io_error)
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

    /// Write one filesystem block from `buf`.
    fn write_block(&self, block: u64, buf: &[u8]) -> Result<(), FsError> {
        let size = self.block_size as usize;
        if block >= u64::from(self.blocks_count) || buf.len() < size {
            return Err(FsError::Invalid);
        }
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
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
impl Ext2 {
    /// Allocate a data block from the first group with a free bit, updating
    /// the group descriptor and the superblock counters together.
    fn alloc_block(&self) -> Result<u32, FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        let size = self.block_size as usize;
        for group in 0..self.groups {
            let desc = self.read_group(group)?;
            if desc.free_blocks == 0 {
                continue;
            }
            let base = self.first_data_block + group * self.blocks_per_group;
            if base >= self.blocks_count {
                continue;
            }
            let bits = min(self.blocks_per_group, self.blocks_count - base);
            let mut bitmap = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(desc.block_bitmap), &mut bitmap[..size])?;
            let Some(index) = Self::bitmap_find_zero(&bitmap[..size], 0, bits) else {
                continue; // counts and bitmap disagree; try the next group
            };
            Self::bitmap_set(&mut bitmap[..size], index);
            self.write_block(u64::from(desc.block_bitmap), &bitmap[..size])?;
            let mut updated = desc;
            updated.free_blocks = updated.free_blocks.checked_sub(1).ok_or(FsError::Invalid)?;
            self.write_group(group, &updated)?;
            self.update_counts(-1, 0)?;
            return Ok(base + index);
        }
        Err(FsError::NoSpace)
    }

    /// Return a data block to its group bitmap and counters.
    fn free_block(&self, block: u32) -> Result<(), FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        if block < self.first_data_block || block >= self.blocks_count {
            return Err(FsError::Invalid);
        }
        let group = (block - self.first_data_block) / self.blocks_per_group;
        let index = (block - self.first_data_block) % self.blocks_per_group;
        let desc = self.read_group(group)?;
        let size = self.block_size as usize;
        let mut bitmap = [0u8; MAX_BLOCK_SIZE];
        self.read_block(u64::from(desc.block_bitmap), &mut bitmap[..size])?;
        if !Self::bitmap_test(&bitmap[..size], index) {
            return Err(FsError::Invalid); // double free: the image is inconsistent
        }
        Self::bitmap_clear(&mut bitmap[..size], index);
        self.write_block(u64::from(desc.block_bitmap), &bitmap[..size])?;
        let mut updated = desc;
        updated.free_blocks = updated.free_blocks.checked_add(1).ok_or(FsError::Invalid)?;
        self.write_group(group, &updated)?;
        self.update_counts(1, 0)
    }

    /// Allocate an inode, updating the group's free count and directory count.
    fn alloc_inode(&self, is_dir: bool) -> Result<u32, FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        let size = self.block_size as usize;
        for group in 0..self.groups {
            let desc = self.read_group(group)?;
            if desc.free_inodes == 0 {
                continue;
            }
            let base = group * self.inodes_per_group;
            if base >= self.inodes_count {
                continue;
            }
            let bits = min(self.inodes_per_group, self.inodes_count - base);
            // Group 0 keeps the reserved inodes below `first_ino`.
            let start = if group == 0 {
                self.first_ino.saturating_sub(1)
            } else {
                0
            };
            if start >= bits {
                continue;
            }
            let mut bitmap = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(desc.inode_bitmap), &mut bitmap[..size])?;
            let Some(index) = Self::bitmap_find_zero(&bitmap[..size], start, bits) else {
                continue;
            };
            Self::bitmap_set(&mut bitmap[..size], index);
            self.write_block(u64::from(desc.inode_bitmap), &bitmap[..size])?;
            let mut updated = desc;
            updated.free_inodes = updated.free_inodes.checked_sub(1).ok_or(FsError::Invalid)?;
            if is_dir {
                updated.used_dirs = updated.used_dirs.checked_add(1).ok_or(FsError::Invalid)?;
            }
            self.write_group(group, &updated)?;
            self.update_counts(0, -1)?;
            return Ok(base + index + 1);
        }
        Err(FsError::NoSpace)
    }

    /// Return an inode to its group bitmap and counters.
    fn free_inode(&self, ino: u32, is_dir: bool) -> Result<(), FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        if ino < ROOT_INO || ino > self.inodes_count {
            return Err(FsError::Invalid);
        }
        let index = ino - 1;
        let group = index / self.inodes_per_group;
        let local = index % self.inodes_per_group;
        let desc = self.read_group(group)?;
        let size = self.block_size as usize;
        let mut bitmap = [0u8; MAX_BLOCK_SIZE];
        self.read_block(u64::from(desc.inode_bitmap), &mut bitmap[..size])?;
        if !Self::bitmap_test(&bitmap[..size], local) {
            return Err(FsError::Invalid); // double free
        }
        Self::bitmap_clear(&mut bitmap[..size], local);
        self.write_block(u64::from(desc.inode_bitmap), &bitmap[..size])?;
        let mut updated = desc;
        updated.free_inodes = updated.free_inodes.checked_add(1).ok_or(FsError::Invalid)?;
        if is_dir {
            updated.used_dirs = updated.used_dirs.checked_sub(1).ok_or(FsError::Invalid)?;
        }
        self.write_group(group, &updated)?;
        self.update_counts(0, 1)
    }

    /// Read the 128-byte core of an inode; larger inode tails are left alone.
    fn read_inode(&self, ino: u32) -> Result<[u8; INODE_CORE_SIZE], FsError> {
        if ino == 0 || ino > self.inodes_count {
            return Err(FsError::Invalid);
        }
        let index = ino - 1;
        let group = index / self.inodes_per_group;
        let local = index % self.inodes_per_group;
        let desc = self.read_group(group)?;
        let block = desc.inode_table as u64 + u64::from(local / self.inodes_per_block);
        let slot = (local % self.inodes_per_block) as usize;
        let size = self.block_size as usize;
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        self.read_block(block, &mut buf[..size])?;
        let offset = slot * usize::from(self.inode_size);
        let end = offset + INODE_CORE_SIZE;
        if end > size {
            return Err(FsError::Invalid);
        }
        let mut core = [0u8; INODE_CORE_SIZE];
        core.copy_from_slice(&buf[offset..end]);
        Ok(core)
    }

    /// Write the 128-byte core of an inode (read-modify-write).
    fn write_inode(&self, ino: u32, inode: &[u8; INODE_CORE_SIZE]) -> Result<(), FsError> {
        if ino == 0 || ino > self.inodes_count {
            return Err(FsError::Invalid);
        }
        let index = ino - 1;
        let group = index / self.inodes_per_group;
        let local = index % self.inodes_per_group;
        let desc = self.read_group(group)?;
        let block = desc.inode_table as u64 + u64::from(local / self.inodes_per_block);
        let slot = (local % self.inodes_per_block) as usize;
        let size = self.block_size as usize;
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        self.read_block(block, &mut buf[..size])?;
        let offset = slot * usize::from(self.inode_size);
        let end = offset + INODE_CORE_SIZE;
        if end > size {
            return Err(FsError::Invalid);
        }
        buf[offset..end].copy_from_slice(inode);
        self.write_block(block, &buf[..size])
    }

    /// The block pointer in direct slot `index`.
    fn direct_ptr(inode: &[u8; INODE_CORE_SIZE], index: u32) -> u32 {
        le32(inode, INO_BLOCK + index as usize * 4)
    }

    /// Resolve logical block `index` through the direct or single-indirect
    /// map; `0` means a hole. Double/triple indirect files are refused rather
    /// than misread.
    fn block_map(&self, inode: &[u8; INODE_CORE_SIZE], index: u32) -> Result<u32, FsError> {
        if index < DIRECT_BLOCKS {
            return Ok(Self::direct_ptr(inode, index));
        }
        if index < DIRECT_BLOCKS + self.ptrs_per_block {
            let indirect = Self::direct_ptr(inode, SINGLE_INDIRECT_SLOT);
            if indirect == 0 {
                return Ok(0);
            }
            let size = self.block_size as usize;
            let mut buf = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(indirect), &mut buf[..size])?;
            return Ok(le32(&buf, ((index - DIRECT_BLOCKS) * 4) as usize));
        }
        if Self::direct_ptr(inode, DOUBLE_INDIRECT_SLOT) != 0
            || Self::direct_ptr(inode, TRIPLE_INDIRECT_SLOT) != 0
        {
            return Err(FsError::NotSupported);
        }
        Ok(0)
    }

    /// Account one newly allocated block in `i_blocks` (512-byte units).
    fn add_inode_sectors(&self, inode: &mut [u8; INODE_CORE_SIZE]) -> Result<(), FsError> {
        let sectors = self.block_size / SECTOR_SIZE as u32;
        let value = le32(inode, INO_BLOCKS)
            .checked_add(sectors)
            .ok_or(FsError::Invalid)?;
        put32(inode, INO_BLOCKS, value);
        Ok(())
    }

    /// Resolve logical block `index` for writing, allocating the data block
    /// (and the indirect block) when missing. Returns `(block, fresh)` so the
    /// caller can zero-fill a brand-new block before the short write.
    fn ensure_block(
        &self,
        inode: &mut [u8; INODE_CORE_SIZE],
        index: u32,
    ) -> Result<(u32, bool), FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        if index < DIRECT_BLOCKS {
            let existing = Self::direct_ptr(inode, index);
            if existing != 0 {
                return Ok((existing, false));
            }
            let fresh = self.alloc_block()?;
            put32(inode, INO_BLOCK + index as usize * 4, fresh);
            self.add_inode_sectors(inode)?;
            return Ok((fresh, true));
        }
        if index < DIRECT_BLOCKS + self.ptrs_per_block {
            let size = self.block_size as usize;
            let mut table = [0u8; MAX_BLOCK_SIZE];
            let mut indirect = Self::direct_ptr(inode, SINGLE_INDIRECT_SLOT);
            if indirect == 0 {
                indirect = self.alloc_block()?;
                self.write_block(u64::from(indirect), &table[..size])?; // zeroed
                put32(
                    inode,
                    INO_BLOCK + SINGLE_INDIRECT_SLOT as usize * 4,
                    indirect,
                );
                self.add_inode_sectors(inode)?;
            } else {
                self.read_block(u64::from(indirect), &mut table[..size])?;
            }
            let slot = ((index - DIRECT_BLOCKS) * 4) as usize;
            let existing = le32(&table, slot);
            if existing != 0 {
                return Ok((existing, false));
            }
            let fresh = self.alloc_block()?;
            put32(&mut table, slot, fresh);
            self.write_block(u64::from(indirect), &table[..size])?;
            self.add_inode_sectors(inode)?;
            return Ok((fresh, true));
        }
        if Self::direct_ptr(inode, DOUBLE_INDIRECT_SLOT) != 0
            || Self::direct_ptr(inode, TRIPLE_INDIRECT_SLOT) != 0
        {
            return Err(FsError::NotSupported);
        }
        Err(FsError::NoSpace)
    }

    /// Free every block a file inode owns (direct, then single indirect).
    fn free_inode_blocks(&self, inode: &[u8; INODE_CORE_SIZE]) -> Result<(), FsError> {
        for slot in 0..DIRECT_BLOCKS {
            let block = Self::direct_ptr(inode, slot);
            if block != 0 {
                self.free_block(block)?;
            }
        }
        let indirect = Self::direct_ptr(inode, SINGLE_INDIRECT_SLOT);
        if indirect != 0 {
            let size = self.block_size as usize;
            let mut table = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(indirect), &mut table[..size])?;
            for index in 0..self.ptrs_per_block {
                let block = le32(&table, index as usize * 4);
                if block != 0 {
                    self.free_block(block)?;
                }
            }
            self.free_block(indirect)?;
        }
        if Self::direct_ptr(inode, DOUBLE_INDIRECT_SLOT) != 0
            || Self::direct_ptr(inode, TRIPLE_INDIRECT_SLOT) != 0
        {
            return Err(FsError::NotSupported);
        }
        Ok(())
    }
}
impl Ext2 {
    /// The physical blocks a directory owns, in logical order. Directories are
    /// dense: a hole is corruption (there is no path that creates one).
    fn dir_blocks(&self, inode: &[u8; INODE_CORE_SIZE]) -> Result<Vec<u32>, FsError> {
        if kind_from_mode(le16(inode, INO_MODE)) != Some(FileKind::Dir) {
            return Err(FsError::NotDir);
        }
        let block_size = u64::from(self.block_size);
        let size = u64::from(le32(inode, INO_SIZE));
        if size == 0 || size % block_size != 0 {
            return Err(FsError::Invalid);
        }
        let count = size / block_size;
        let capacity = u64::from(DIRECT_BLOCKS + self.ptrs_per_block);
        if count > capacity {
            return Err(FsError::NotSupported);
        }
        let mut blocks = Vec::new();
        for index in 0..count as u32 {
            let block = self.block_map(inode, index)?;
            if block == 0 {
                return Err(FsError::Invalid);
            }
            blocks.push(block);
        }
        Ok(blocks)
    }

    /// Resolve `path` within the volume to an inode number, one directory
    /// entry per component from the root.
    fn resolve(&self, path: &str) -> Result<u32, FsError> {
        let mut ino = ROOT_INO;
        for part in path.split('/').filter(|part| !part.is_empty()) {
            if part == "." || part == ".." {
                return Err(FsError::Invalid);
            }
            if part.len() > MAX_NAME {
                return Err(FsError::NameTooLong);
            }
            ino = self.find_entry(ino, part)?.0;
        }
        Ok(ino)
    }

    /// Find `name` in directory `dir_ino` as `(inode, file type byte)`.
    fn find_entry(&self, dir_ino: u32, name: &str) -> Result<(u32, u8), FsError> {
        let inode = self.read_inode(dir_ino)?;
        let blocks = self.dir_blocks(&inode)?;
        let size = self.block_size as usize;
        for block in blocks {
            let mut buf = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(block), &mut buf[..size])?;
            let mut offset = 0usize;
            for _ in 0..(size / DE_HEADER) {
                if offset + DE_HEADER > size {
                    break;
                }
                let entry_ino = le32(&buf, offset + DE_INO);
                let rec_len = le16(&buf, offset + DE_REC_LEN) as usize;
                let name_len = buf[offset + DE_NAME_LEN] as usize;
                if rec_len < DE_HEADER
                    || rec_len % 4 != 0
                    || offset + rec_len > size
                    || name_len > rec_len - DE_HEADER
                {
                    return Err(FsError::Invalid);
                }
                let entry_name = &buf[offset + DE_HEADER..offset + DE_HEADER + name_len];
                if entry_ino != 0 && entry_name == name.as_bytes() {
                    return Ok((entry_ino, buf[offset + DE_FILE_TYPE]));
                }
                offset += rec_len;
                if offset == size {
                    break;
                }
            }
        }
        Err(FsError::NotFound)
    }

    /// Whether directory `dir_ino` holds no entries besides `.` and `..`.
    fn dir_is_empty(&self, dir_ino: u32) -> Result<bool, FsError> {
        let inode = self.read_inode(dir_ino)?;
        let blocks = self.dir_blocks(&inode)?;
        let size = self.block_size as usize;
        for block in blocks {
            let mut buf = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(block), &mut buf[..size])?;
            let mut offset = 0usize;
            for _ in 0..(size / DE_HEADER) {
                if offset + DE_HEADER > size {
                    break;
                }
                let entry_ino = le32(&buf, offset + DE_INO);
                let rec_len = le16(&buf, offset + DE_REC_LEN) as usize;
                let name_len = buf[offset + DE_NAME_LEN] as usize;
                if rec_len < DE_HEADER
                    || rec_len % 4 != 0
                    || offset + rec_len > size
                    || name_len > rec_len - DE_HEADER
                {
                    return Err(FsError::Invalid);
                }
                if entry_ino != 0 {
                    let name = &buf[offset + DE_HEADER..offset + DE_HEADER + name_len];
                    if name != b"." && name != b".." {
                        return Ok(false);
                    }
                }
                offset += rec_len;
                if offset == size {
                    break;
                }
            }
        }
        Ok(true)
    }

    /// Add `name` -> `child_ino` to directory `dir_ino`, splitting a free
    /// record or appending a fresh block. `dir` is the caller's in-memory
    /// inode; it is written back with a fresh `ctime`/`mtime`.
    fn add_entry(
        &self,
        dir_ino: u32,
        dir: &mut [u8; INODE_CORE_SIZE],
        name: &str,
        child_ino: u32,
        file_type: u8,
    ) -> Result<(), FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        if name.is_empty() || name.len() > MAX_NAME {
            return Err(FsError::NameTooLong);
        }
        let size = self.block_size as usize;
        let needed = (DE_HEADER + name.len() + 3) & !3;
        if needed > size {
            return Err(FsError::NameTooLong);
        }
        let blocks = self.dir_blocks(dir)?;
        for block in blocks {
            let mut buf = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(block), &mut buf[..size])?;
            let mut offset = 0usize;
            for _ in 0..(size / DE_HEADER) {
                if offset + DE_HEADER > size {
                    break;
                }
                let entry_ino = le32(&buf, offset + DE_INO);
                let rec_len = le16(&buf, offset + DE_REC_LEN) as usize;
                let name_len = buf[offset + DE_NAME_LEN] as usize;
                if rec_len < DE_HEADER
                    || rec_len % 4 != 0
                    || offset + rec_len > size
                    || name_len > rec_len - DE_HEADER
                {
                    return Err(FsError::Invalid);
                }
                // An entry can take a free record, or be carved out of a
                // record that has slack after its own name (the trailing `..`
                // in a fresh directory is exactly that case). This is Linux's
                // `ext2_add_link` rule.
                let own = (DE_HEADER + name_len + 3) & !3;
                if entry_ino == 0 && rec_len >= needed {
                    put32(&mut buf, offset + DE_INO, child_ino);
                    put16(&mut buf, offset + DE_REC_LEN, rec_len as u16);
                    buf[offset + DE_NAME_LEN] = name.len() as u8;
                    buf[offset + DE_FILE_TYPE] = file_type;
                    let name_offset = offset + DE_HEADER;
                    buf[name_offset..name_offset + name.len()].copy_from_slice(name.as_bytes());
                    self.write_block(u64::from(block), &buf[..size])?;
                    touch(dir, now());
                    return self.write_inode(dir_ino, dir);
                }
                if rec_len >= own + needed {
                    let new_offset = offset + own;
                    put16(&mut buf, offset + DE_REC_LEN, own as u16);
                    put32(&mut buf, new_offset + DE_INO, child_ino);
                    put16(&mut buf, new_offset + DE_REC_LEN, (rec_len - own) as u16);
                    buf[new_offset + DE_NAME_LEN] = name.len() as u8;
                    buf[new_offset + DE_FILE_TYPE] = file_type;
                    let name_offset = new_offset + DE_HEADER;
                    buf[name_offset..name_offset + name.len()].copy_from_slice(name.as_bytes());
                    self.write_block(u64::from(block), &buf[..size])?;
                    touch(dir, now());
                    return self.write_inode(dir_ino, dir);
                }
                offset += rec_len;
                if offset == size {
                    break;
                }
            }
        }

        // No free slot: append a block and make it one whole-block entry.
        let index = le32(dir, INO_SIZE) / self.block_size;
        if index >= DIRECT_BLOCKS + self.ptrs_per_block {
            return Err(FsError::NoSpace);
        }
        let (block, fresh) = self.ensure_block(dir, index)?;
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        if !fresh {
            self.read_block(u64::from(block), &mut buf[..size])?;
        }
        put32(&mut buf, DE_INO, child_ino);
        put16(&mut buf, DE_REC_LEN, size as u16);
        buf[DE_NAME_LEN] = name.len() as u8;
        buf[DE_FILE_TYPE] = file_type;
        buf[DE_HEADER..DE_HEADER + name.len()].copy_from_slice(name.as_bytes());
        self.write_block(u64::from(block), &buf[..size])?;
        let new_size = le32(dir, INO_SIZE)
            .checked_add(self.block_size)
            .ok_or(FsError::NoSpace)?;
        put32(dir, INO_SIZE, new_size);
        touch(dir, now());
        self.write_inode(dir_ino, dir)
    }

    /// Remove `name` from directory `dir_ino` (merging the record into its
    /// predecessor when possible) and return the child's inode number.
    fn remove_entry(
        &self,
        dir_ino: u32,
        dir: &mut [u8; INODE_CORE_SIZE],
        name: &str,
    ) -> Result<u32, FsError> {
        if self.read_only {
            return Err(FsError::ReadOnly);
        }
        let size = self.block_size as usize;
        let blocks = self.dir_blocks(dir)?;
        for block in blocks {
            let mut buf = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(block), &mut buf[..size])?;
            let mut offset = 0usize;
            let mut previous = 0usize;
            for _ in 0..(size / DE_HEADER) {
                if offset + DE_HEADER > size {
                    break;
                }
                let entry_ino = le32(&buf, offset + DE_INO);
                let rec_len = le16(&buf, offset + DE_REC_LEN) as usize;
                let name_len = buf[offset + DE_NAME_LEN] as usize;
                if rec_len < DE_HEADER
                    || rec_len % 4 != 0
                    || offset + rec_len > size
                    || name_len > rec_len - DE_HEADER
                {
                    return Err(FsError::Invalid);
                }
                let entry_name = &buf[offset + DE_HEADER..offset + DE_HEADER + name_len];
                if entry_ino != 0 && entry_name == name.as_bytes() {
                    if offset == 0 {
                        put32(&mut buf, DE_INO, 0); // first record: leave a hole
                    } else {
                        let previous_len = le16(&buf, previous + DE_REC_LEN) as usize;
                        put16(
                            &mut buf,
                            previous + DE_REC_LEN,
                            (previous_len + rec_len) as u16,
                        );
                    }
                    self.write_block(u64::from(block), &buf[..size])?;
                    touch(dir, now());
                    self.write_inode(dir_ino, dir)?;
                    return Ok(entry_ino);
                }
                previous = offset;
                offset += rec_len;
                if offset == size {
                    break;
                }
            }
        }
        Err(FsError::NotFound)
    }

    /// Point the `..` entry of directory `child` at `parent_ino`.
    fn set_dotdot(
        &self,
        child_ino: u32,
        child: &mut [u8; INODE_CORE_SIZE],
        parent_ino: u32,
    ) -> Result<(), FsError> {
        let size = self.block_size as usize;
        let block = self.block_map(child, 0)?;
        if block == 0 {
            return Err(FsError::Invalid);
        }
        let mut buf = [0u8; MAX_BLOCK_SIZE];
        self.read_block(u64::from(block), &mut buf[..size])?;
        let mut offset = 0usize;
        for _ in 0..(size / DE_HEADER) {
            if offset + DE_HEADER > size {
                break;
            }
            let rec_len = le16(&buf, offset + DE_REC_LEN) as usize;
            let name_len = buf[offset + DE_NAME_LEN] as usize;
            if rec_len < DE_HEADER
                || rec_len % 4 != 0
                || offset + rec_len > size
                || name_len > rec_len - DE_HEADER
            {
                return Err(FsError::Invalid);
            }
            if &buf[offset + DE_HEADER..offset + DE_HEADER + name_len] == b".." {
                put32(&mut buf, offset + DE_INO, parent_ino);
                self.write_block(u64::from(block), &buf[..size])?;
                touch(child, now());
                return self.write_inode(child_ino, child);
            }
            offset += rec_len;
            if offset == size {
                break;
            }
        }
        Err(FsError::Invalid) // every directory must carry `.` and `..`
    }

    /// Whether `node` lies inside directory `ancestor` (walking `..` up).
    /// Used to refuse moving a directory into itself.
    fn is_within(&self, ancestor: u32, node: u32) -> Result<bool, FsError> {
        let mut current = node;
        let mut steps = 0u32;
        while current != ROOT_INO {
            if current == ancestor {
                return Ok(true);
            }
            let inode = self.read_inode(current)?;
            if kind_from_mode(le16(&inode, INO_MODE)) != Some(FileKind::Dir) {
                return Ok(false);
            }
            current = self.find_entry(current, "..")?.0;
            steps += 1;
            if steps > self.inodes_count {
                return Err(FsError::Invalid); // a `..` cycle
            }
        }
        Ok(false)
    }

    /// The size of a regular file, honouring the large-file high bits.
    fn file_size(&self, inode: &[u8; INODE_CORE_SIZE]) -> u64 {
        let low = u64::from(le32(inode, INO_SIZE));
        if self.has_large_file {
            low | (u64::from(le32(inode, INO_DIR_ACL)) << 32)
        } else {
            low
        }
    }

    /// Build [`Meta`] for an inode number.
    fn meta_of(&self, ino: u32) -> Result<Meta, FsError> {
        let inode = self.read_inode(ino)?;
        let mode = le16(&inode, INO_MODE);
        let kind = kind_from_mode(mode).ok_or(FsError::NotSupported)?;
        let size = if kind == FileKind::File {
            self.file_size(&inode)
        } else {
            u64::from(le32(&inode, INO_SIZE))
        };
        Ok(Meta {
            ino: u64::from(ino),
            mode,
            uid: u32::from(le16(&inode, INO_UID)),
            gid: u32::from(le16(&inode, INO_GID)),
            size,
            kind,
        })
    }
}
impl Filesystem for Ext2 {
    fn name(&self) -> &'static str {
        "ext2 (rw)"
    }

    fn lookup(&self, path: &str) -> Result<Meta, FsError> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        self.meta_of(ino)
    }

    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        let inode = self.read_inode(ino)?;
        if kind_from_mode(le16(&inode, INO_MODE)) != Some(FileKind::File) {
            return Err(FsError::IsDir);
        }
        let size = self.file_size(&inode);
        if offset >= size || buf.is_empty() {
            return Ok(0);
        }
        let count = min(size - offset, buf.len() as u64) as usize;
        let block_size = u64::from(self.block_size);
        let size_usize = self.block_size as usize;
        let mut done = 0usize;
        while done < count {
            let position = offset + done as u64;
            let index = (position / block_size) as u32;
            let inner = (position % block_size) as usize;
            let chunk = min(size_usize - inner, count - done);
            let block = self.block_map(&inode, index)?;
            if block == 0 {
                buf[done..done + chunk].fill(0); // a sparse hole reads as zero
            } else {
                let mut tmp = [0u8; MAX_BLOCK_SIZE];
                self.read_block(u64::from(block), &mut tmp[..size_usize])?;
                buf[done..done + chunk].copy_from_slice(&tmp[inner..inner + chunk]);
            }
            done += chunk;
        }
        Ok(done)
    }

    fn write(&self, path: &str, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        let mut inode = self.read_inode(ino)?;
        if kind_from_mode(le16(&inode, INO_MODE)) != Some(FileKind::File) {
            return Err(FsError::IsDir);
        }
        if data.is_empty() {
            return Ok(0);
        }
        offset
            .checked_add(data.len() as u64)
            .ok_or(FsError::NoSpace)?;
        let block_size = u64::from(self.block_size);
        let size_usize = self.block_size as usize;
        let mut done = 0usize;
        let mut failure = None;
        while done < data.len() {
            let position = offset + done as u64;
            let index = (position / block_size) as u32;
            let inner = (position % block_size) as usize;
            let chunk = min(size_usize - inner, data.len() - done);
            let (block, fresh) = match self.ensure_block(&mut inode, index) {
                Ok(mapped) => mapped,
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            };
            let mut tmp = [0u8; MAX_BLOCK_SIZE];
            if !fresh {
                if let Err(error) = self.read_block(u64::from(block), &mut tmp[..size_usize]) {
                    failure = Some(error);
                    break;
                }
            }
            // A fresh block is written from zeros, so a short write can never
            // expose stale bytes from the block's previous owner.
            tmp[inner..inner + chunk].copy_from_slice(&data[done..done + chunk]);
            if let Err(error) = self.write_block(u64::from(block), &tmp[..size_usize]) {
                failure = Some(error);
                break;
            }
            done += chunk;
        }
        // `ensure_block` allocated blocks and edited the in-memory inode as it
        // went. Persist the inode whatever happened, or every block allocated
        // before a failure (out of space, an I/O error) stays marked used in
        // the bitmap while no inode owns it: a permanent leak, and the bytes
        // already written vanish. The size covers exactly what landed.
        let landed = offset + done as u64;
        if done > 0 && landed > self.file_size(&inode) {
            put32(&mut inode, INO_SIZE, landed as u32);
        }
        touch(&mut inode, now());
        let persisted = self.write_inode(ino, &inode);
        match failure {
            // A short write reports the bytes that landed; the caller's next
            // write sees the failure again with nothing written.
            Some(error) if done == 0 => Err(error),
            _ => persisted.map(|()| done),
        }
    }

    fn create(&self, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError> {
        let _guard = self.lock.lock();
        let (parent_path, name) = split_parent(path)?;
        let parent_ino = self.resolve(parent_path)?;
        let mut parent = self.read_inode(parent_ino)?;
        if kind_from_mode(le16(&parent, INO_MODE)) != Some(FileKind::Dir) {
            return Err(FsError::NotDir);
        }
        if self.find_entry(parent_ino, name).is_ok() {
            return Err(FsError::Exists);
        }
        check_owner(owner)?;
        let ino = self.alloc_inode(false)?;
        let mut inode = [0u8; INODE_CORE_SIZE];
        put16(&mut inode, INO_MODE, S_IFREG | (mode & 0o7777));
        put16(&mut inode, INO_UID, owner.uid as u16);
        put16(&mut inode, INO_GID, owner.gid as u16);
        put16(&mut inode, INO_LINKS, 1);
        let time = now();
        put32(&mut inode, INO_ATIME, time);
        touch(&mut inode, time);
        self.write_inode(ino, &inode)?;
        match self.add_entry(parent_ino, &mut parent, name, ino, FT_REGULAR) {
            Ok(()) => self.meta_of(ino),
            Err(error) => {
                // Roll the fresh inode back; the parent was not written.
                let _ = self.free_inode(ino, false);
                Err(error)
            }
        }
    }

    fn mkdir(&self, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError> {
        let _guard = self.lock.lock();
        let (parent_path, name) = split_parent(path)?;
        let parent_ino = self.resolve(parent_path)?;
        let mut parent = self.read_inode(parent_ino)?;
        if kind_from_mode(le16(&parent, INO_MODE)) != Some(FileKind::Dir) {
            return Err(FsError::NotDir);
        }
        if self.find_entry(parent_ino, name).is_ok() {
            return Err(FsError::Exists);
        }
        check_owner(owner)?;
        let ino = self.alloc_inode(true)?;
        let block = match self.alloc_block() {
            Ok(block) => block,
            Err(error) => {
                let _ = self.free_inode(ino, true);
                return Err(error);
            }
        };
        let size = self.block_size as usize;
        let mut dir = [0u8; MAX_BLOCK_SIZE];
        put32(&mut dir, DE_INO, ino);
        put16(&mut dir, DE_REC_LEN, 12);
        dir[DE_NAME_LEN] = 1;
        dir[DE_FILE_TYPE] = FT_DIRECTORY;
        dir[DE_HEADER] = b'.';
        let dotdot = DE_HEADER + 4; // aligned start of the `..` record
        put32(&mut dir, dotdot + DE_INO, parent_ino);
        put16(&mut dir, dotdot + DE_REC_LEN, (size - dotdot) as u16);
        dir[dotdot + DE_NAME_LEN] = 2;
        dir[dotdot + DE_FILE_TYPE] = FT_DIRECTORY;
        dir[dotdot + DE_HEADER] = b'.';
        dir[dotdot + DE_HEADER + 1] = b'.';
        if let Err(error) = self.write_block(u64::from(block), &dir[..size]) {
            let _ = self.free_block(block);
            let _ = self.free_inode(ino, true);
            return Err(error);
        }

        let mut inode = [0u8; INODE_CORE_SIZE];
        put16(&mut inode, INO_MODE, S_IFDIR | (mode & 0o7777));
        put16(&mut inode, INO_UID, owner.uid as u16);
        put16(&mut inode, INO_GID, owner.gid as u16);
        put32(&mut inode, INO_SIZE, self.block_size);
        put16(&mut inode, INO_LINKS, 2); // `.` and the parent's entry
        put32(&mut inode, INO_BLOCKS, self.block_size / SECTOR_SIZE as u32);
        put32(&mut inode, INO_BLOCK, block);
        let time = now();
        put32(&mut inode, INO_ATIME, time);
        touch(&mut inode, time);
        self.write_inode(ino, &inode)?;

        // The new child makes the parent worth one more link.
        let links = le16(&parent, INO_LINKS)
            .checked_add(1)
            .ok_or(FsError::Invalid)?;
        put16(&mut parent, INO_LINKS, links);
        match self.add_entry(parent_ino, &mut parent, name, ino, FT_DIRECTORY) {
            Ok(()) => self.meta_of(ino),
            Err(error) => {
                let _ = self.free_block(block);
                let _ = self.free_inode(ino, true);
                Err(error)
            }
        }
    }

    fn unlink(&self, path: &str) -> Result<(), FsError> {
        let _guard = self.lock.lock();
        let (parent_path, name) = split_parent(path)?;
        let parent_ino = self.resolve(parent_path)?;
        let mut parent = self.read_inode(parent_ino)?;
        if kind_from_mode(le16(&parent, INO_MODE)) != Some(FileKind::Dir) {
            return Err(FsError::NotDir);
        }
        let (child_ino, _) = self.find_entry(parent_ino, name)?;
        let mut child = self.read_inode(child_ino)?;
        if kind_from_mode(le16(&child, INO_MODE)) != Some(FileKind::File) {
            return Err(FsError::IsDir);
        }
        self.remove_entry(parent_ino, &mut parent, name)?;
        let links = le16(&child, INO_LINKS);
        if links <= 1 {
            // Last link: release the data blocks, then the inode itself.
            self.free_inode_blocks(&child)?;
            put16(&mut child, INO_LINKS, 0);
            put32(&mut child, INO_DTIME, now());
            self.write_inode(child_ino, &child)?;
            self.free_inode(child_ino, false)?;
        } else {
            put16(&mut child, INO_LINKS, links - 1);
            touch(&mut child, now());
            self.write_inode(child_ino, &child)?;
        }
        Ok(())
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        let _guard = self.lock.lock();
        if from == to {
            return Ok(());
        }
        let (from_parent_path, from_name) = split_parent(from)?;
        let (to_parent_path, to_name) = split_parent(to)?;
        let from_parent_ino = self.resolve(from_parent_path)?;
        let to_parent_ino = self.resolve(to_parent_path)?;
        let mut from_parent = self.read_inode(from_parent_ino)?;
        if kind_from_mode(le16(&from_parent, INO_MODE)) != Some(FileKind::Dir) {
            return Err(FsError::NotDir);
        }
        let mut to_parent = self.read_inode(to_parent_ino)?;
        if kind_from_mode(le16(&to_parent, INO_MODE)) != Some(FileKind::Dir) {
            return Err(FsError::NotDir);
        }
        let (child_ino, _) = self.find_entry(from_parent_ino, from_name)?;
        let mut child = self.read_inode(child_ino)?;
        let child_kind = kind_from_mode(le16(&child, INO_MODE)).ok_or(FsError::NotSupported)?;
        // Moving a directory below itself would make a cycle.
        if child_kind == FileKind::Dir && self.is_within(child_ino, to_parent_ino)? {
            return Err(FsError::Invalid);
        }

        // The destination may exist: a file replaces a file, a directory may
        // replace only an empty directory (the same rules as ramfs).
        if let Ok((existing, _)) = self.find_entry(to_parent_ino, to_name) {
            if existing == child_ino {
                return Ok(()); // already linked there
            }
            let mut victim = self.read_inode(existing)?;
            let victim_kind =
                kind_from_mode(le16(&victim, INO_MODE)).ok_or(FsError::NotSupported)?;
            match (child_kind, victim_kind) {
                (FileKind::File, FileKind::File) => {}
                (FileKind::Dir, FileKind::Dir) => {
                    if !self.dir_is_empty(existing)? {
                        return Err(FsError::NotEmpty);
                    }
                }
                (FileKind::File, FileKind::Dir) => return Err(FsError::IsDir),
                (FileKind::Dir, FileKind::File) => return Err(FsError::NotDir),
            }
            self.remove_entry(to_parent_ino, &mut to_parent, to_name)?;
            let links = le16(&victim, INO_LINKS);
            if links <= 1 {
                self.free_inode_blocks(&victim)?;
                put16(&mut victim, INO_LINKS, 0);
                put32(&mut victim, INO_DTIME, now());
                self.write_inode(existing, &victim)?;
                self.free_inode(existing, victim_kind == FileKind::Dir)?;
            } else {
                put16(&mut victim, INO_LINKS, links - 1);
                touch(&mut victim, now());
                self.write_inode(existing, &victim)?;
            }
        }

        // Directory bookkeeping: the old parent loses a child directory, the
        // new parent gains one, and the moved directory's `..` follows.
        if child_kind == FileKind::Dir && from_parent_ino != to_parent_ino {
            let from_links = le16(&from_parent, INO_LINKS).saturating_sub(1);
            put16(&mut from_parent, INO_LINKS, from_links);
            let to_links = le16(&to_parent, INO_LINKS)
                .checked_add(1)
                .ok_or(FsError::Invalid)?;
            put16(&mut to_parent, INO_LINKS, to_links);
            self.set_dotdot(child_ino, &mut child, to_parent_ino)?;
        }
        if child_kind == FileKind::File {
            touch(&mut child, now());
            self.write_inode(child_ino, &child)?;
        }

        self.remove_entry(from_parent_ino, &mut from_parent, from_name)?;
        let file_type = if child_kind == FileKind::Dir {
            FT_DIRECTORY
        } else {
            FT_REGULAR
        };
        if let Err(error) =
            self.add_entry(to_parent_ino, &mut to_parent, to_name, child_ino, file_type)
        {
            // Put the source entry back so a failure leaves the tree intact.
            let _ = self.add_entry(
                from_parent_ino,
                &mut from_parent,
                from_name,
                child_ino,
                file_type,
            );
            return Err(error);
        }
        Ok(())
    }

    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        let _guard = self.lock.lock();
        let ino = self.resolve(path)?;
        let inode = self.read_inode(ino)?;
        let blocks = self.dir_blocks(&inode)?;
        let size = self.block_size as usize;
        let mut entries = Vec::new();
        for block in blocks {
            let mut buf = [0u8; MAX_BLOCK_SIZE];
            self.read_block(u64::from(block), &mut buf[..size])?;
            let mut offset = 0usize;
            for _ in 0..(size / DE_HEADER) {
                if offset + DE_HEADER > size {
                    break;
                }
                let entry_ino = le32(&buf, offset + DE_INO);
                let rec_len = le16(&buf, offset + DE_REC_LEN) as usize;
                let name_len = buf[offset + DE_NAME_LEN] as usize;
                if rec_len < DE_HEADER
                    || rec_len % 4 != 0
                    || offset + rec_len > size
                    || name_len > rec_len - DE_HEADER
                {
                    return Err(FsError::Invalid);
                }
                if entry_ino != 0 && name_len > 0 {
                    let name = &buf[offset + DE_HEADER..offset + DE_HEADER + name_len];
                    if name != b"." && name != b".." {
                        // Revision-0 entries carry no type byte: read the
                        // child inode instead. Types the VFS cannot hold
                        // (symlink, device, ...) are skipped, never guessed.
                        let file_type = buf[offset + DE_FILE_TYPE];
                        let kind = if self.has_file_type && file_type != 0 {
                            match file_type {
                                FT_REGULAR => Some(FileKind::File),
                                FT_DIRECTORY => Some(FileKind::Dir),
                                _ => None,
                            }
                        } else {
                            let child = self.read_inode(entry_ino)?;
                            kind_from_mode(le16(&child, INO_MODE))
                        };
                        if let Some(kind) = kind {
                            entries.push(DirEntry {
                                name: String::from_utf8_lossy(name).into_owned(),
                                ino: u64::from(entry_ino),
                                kind,
                            });
                        }
                    }
                }
                offset += rec_len;
                if offset == size {
                    break;
                }
            }
        }
        Ok(entries)
    }
}
