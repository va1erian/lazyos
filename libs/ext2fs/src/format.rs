//! The formatter: write an empty revision-1 ext2 volume.
//!
//! The result is what `tools/mkdisk` produces and `mke2fs -t ext2 -r 1 -O
//! sparse_super,large_file,filetype` would: filetype directory entries, sparse
//! superblock backups, large files, a root directory (mode `0755`, root
//! owned) and `lost+found`. Everything is derived from [`Geometry`], so the
//! same call builds a test image in memory and the OS volume at build time.
//!
//! The root and `lost+found` take the first data blocks of group 0 and
//! inodes 2 and 11, so the used blocks and inodes stay a contiguous prefix of
//! their bitmaps. Backup copies of the superblock are written but the driver
//! never updates them (it has no use for them; `e2fsck` restores from them).

use super::*;
use crate::geometry::{plan, GroupLayout, Plan};

/// `s_errors`: carry on after an error (the mke2fs default).
const ERRORS_CONTINUE: u16 = 1;
/// `s_max_mnt_count` of -1: never force an fsck by mount count.
const NO_MAX_MOUNT_COUNT: u16 = 0xFFFF;
const SB_LOG_FRAG_SIZE: usize = 0x1C;
const SB_FRAGS_PER_GROUP: usize = 0x24;
const SB_MAX_MNT: usize = 0x36;
const SB_ERRORS: usize = 0x3C;
const SB_LASTCHECK: usize = 0x40;
const SB_BLOCK_GROUP_NR: usize = 0x5A;
/// Longest volume label (`s_volume_name`).
const LABEL_MAX: usize = 16;
/// Zero this many blocks per write when clearing inode tables.
const ZERO_RUN: u64 = 16;

/// Write an empty volume described by `geometry` onto `io`, stamped `now` and
/// carrying `label` (at most 16 ASCII bytes) and `uuid`. The volume is left
/// clean. Open it afterwards with [`Ext2::open`].
pub fn format(
    io: &dyn BlockIo,
    geometry: Geometry,
    label: &str,
    uuid: [u8; 16],
    now: i64,
) -> Result<(), Ext2Error> {
    if label.len() > LABEL_MAX || !label.is_ascii() {
        return Err(Ext2Error::Invalid);
    }
    let plan = plan(&geometry)?;
    let bytes = u64::from(plan.blocks_count) * u64::from(plan.block_size);
    if io.sector_count().saturating_mul(SECTOR_SIZE as u64) < bytes {
        return Err(Ext2Error::Invalid);
    }
    if !io.is_writable() {
        return Err(Ext2Error::ReadOnly);
    }
    let mut name = [0u8; LABEL_MAX];
    name[..label.len()].copy_from_slice(label.as_bytes());
    Formatter {
        io,
        plan,
        stamp: attr::disk_time(now),
        uuid,
        label: name,
    }
    .write()
}

/// The state shared by every step of one format.
struct Formatter<'a> {
    io: &'a dyn BlockIo,
    plan: Plan,
    stamp: u32,
    uuid: [u8; 16],
    label: [u8; LABEL_MAX],
}

/// Block and inode counts of a freshly formatted volume, per group.
struct Usage {
    /// Used blocks: the group's metadata, plus group 0's directories.
    used_blocks: Vec<u32>,
    free_blocks: Vec<u32>,
    free_inodes: Vec<u32>,
}

impl Formatter<'_> {
    fn write(&self) -> Result<(), Ext2Error> {
        let plan = &self.plan;
        let groups: Vec<GroupLayout> = (0..plan.groups).map(|g| plan.group(g)).collect();
        let lost_found = plan.lost_found_blocks();
        let usage = self.usage(&groups, lost_found)?;
        let descriptors = self.descriptors(&groups, &usage);
        let free_blocks: u32 = usage.free_blocks.iter().sum();
        let free_inodes: u32 = usage.free_inodes.iter().sum();
        for (index, group) in groups.iter().enumerate() {
            if group.has_backup {
                let sb = self.superblock(index as u32, free_blocks, free_inodes);
                self.put_bytes(self.sb_position(group), &sb)?;
                self.put_bytes(u64::from(group.start + 1) * self.bs(), &descriptors)?;
            }
            let blocks = self.block_bitmap(group, usage.used_blocks[index]);
            self.put_block(group.block_bitmap, &blocks)?;
            let used_inodes = if index == 0 { DEFAULT_FIRST_INO } else { 0 };
            self.put_block(group.inode_bitmap, &self.inode_bitmap(used_inodes))?;
            self.zero_blocks(group.inode_table, plan.inode_table_blocks)?;
        }
        self.write_directories(&groups[0], lost_found)?;
        self.io.flush().map_err(io_error)
    }

    /// Where group `group`'s superblock starts, in bytes. The primary lives at
    /// byte 1024, wherever its block starts; backups sit at their group start.
    fn sb_position(&self, group: &GroupLayout) -> u64 {
        if group.start == self.plan.first_data_block {
            SUPER_OFFSET
        } else {
            u64::from(group.start) * self.bs()
        }
    }

    fn bs(&self) -> u64 {
        u64::from(self.plan.block_size)
    }

    /// Per-group used and free counts, failing if the volume cannot hold its
    /// own metadata plus the root and `lost+found`.
    fn usage(&self, groups: &[GroupLayout], lost_found: u32) -> Result<Usage, Ext2Error> {
        let mut used_blocks: Vec<u32> = groups.iter().map(|g| g.metadata_blocks()).collect();
        used_blocks[0] += 1 + lost_found; // the root's block and lost+found's
        let free_blocks = groups
            .iter()
            .zip(&used_blocks)
            .map(|(group, used)| group.blocks.checked_sub(*used).ok_or(Ext2Error::NoSpace))
            .collect::<Result<Vec<u32>, _>>()?;
        let mut free_inodes = alloc::vec![self.plan.inodes_per_group; groups.len()];
        free_inodes[0] = free_inodes[0]
            .checked_sub(DEFAULT_FIRST_INO)
            .ok_or(Ext2Error::NoSpace)?;
        Ok(Usage {
            used_blocks,
            free_blocks,
            free_inodes,
        })
    }

    /// The descriptor table, padded to whole blocks.
    fn descriptors(&self, groups: &[GroupLayout], usage: &Usage) -> Vec<u8> {
        let size = (self.plan.gdt_blocks * self.plan.block_size) as usize;
        let mut table = alloc::vec![0u8; size];
        for (index, group) in groups.iter().enumerate() {
            let at = index * GD_SIZE;
            put32(&mut table, at + GD_BLOCK_BITMAP, group.block_bitmap);
            put32(&mut table, at + GD_INODE_BITMAP, group.inode_bitmap);
            put32(&mut table, at + GD_INODE_TABLE, group.inode_table);
            put16(
                &mut table,
                at + GD_FREE_BLOCKS,
                usage.free_blocks[index] as u16,
            );
            put16(
                &mut table,
                at + GD_FREE_INODES,
                usage.free_inodes[index] as u16,
            );
            // Group 0 holds the root and `lost+found`.
            put16(
                &mut table,
                at + GD_USED_DIRS,
                if index == 0 { 2 } else { 0 },
            );
        }
        table
    }

    /// The 1 KiB superblock; backups differ only in `s_block_group_nr`.
    fn superblock(&self, group: u32, free_blocks: u32, free_inodes: u32) -> [u8; 1024] {
        let plan = &self.plan;
        let mut sb = [0u8; 1024];
        let log = plan.block_size.trailing_zeros() - 10; // 1024 -> 0, 2048 -> 1, 4096 -> 2
        put32(&mut sb, SB_INODES_COUNT, plan.inodes_count());
        put32(&mut sb, SB_BLOCKS_COUNT, plan.blocks_count);
        put32(&mut sb, SB_FREE_BLOCKS, free_blocks);
        put32(&mut sb, SB_FREE_INODES, free_inodes);
        put32(&mut sb, SB_FIRST_DATA_BLOCK, plan.first_data_block);
        put32(&mut sb, SB_LOG_BLOCK_SIZE, log);
        put32(&mut sb, SB_LOG_FRAG_SIZE, log);
        put32(&mut sb, SB_BLOCKS_PER_GROUP, plan.blocks_per_group);
        put32(&mut sb, SB_FRAGS_PER_GROUP, plan.blocks_per_group);
        put32(&mut sb, SB_INODES_PER_GROUP, plan.inodes_per_group);
        put32(&mut sb, SB_WTIME, self.stamp);
        put16(&mut sb, SB_MAX_MNT, NO_MAX_MOUNT_COUNT);
        put16(&mut sb, SB_MAGIC, EXT2_MAGIC);
        put16(&mut sb, SB_STATE, STATE_VALID);
        put16(&mut sb, SB_ERRORS, ERRORS_CONTINUE);
        put32(&mut sb, SB_LASTCHECK, self.stamp);
        put32(&mut sb, SB_REV_LEVEL, 1);
        put32(&mut sb, SB_FIRST_INO, DEFAULT_FIRST_INO);
        put16(&mut sb, SB_INODE_SIZE, INODE_CORE_SIZE as u16);
        put16(&mut sb, SB_BLOCK_GROUP_NR, group as u16);
        put32(&mut sb, SB_FEATURE_INCOMPAT, FEATURE_INCOMPAT_FILETYPE);
        put32(
            &mut sb,
            SB_FEATURE_RO_COMPAT,
            FEATURE_RO_SPARSE_SUPER | FEATURE_RO_LARGE_FILE,
        );
        sb[SB_UUID..SB_UUID + 16].copy_from_slice(&self.uuid);
        sb[SB_VOLUME_NAME..SB_VOLUME_NAME + 16].copy_from_slice(&self.label);
        sb
    }

    /// Used blocks are a prefix of the group (metadata, then group-0 data).
    fn block_bitmap(&self, group: &GroupLayout, used: u32) -> Vec<u8> {
        let mut bitmap = alloc::vec![0u8; self.plan.block_size as usize];
        set_bits(&mut bitmap, 0, used);
        // A short last group leaves bits past the volume end; mke2fs marks them
        // used so they can never be allocated.
        let padding = self.plan.blocks_per_group - group.blocks;
        set_bits(&mut bitmap, group.blocks, padding);
        bitmap
    }

    /// The first `used` inodes are taken; the padding past the group's inodes is set.
    fn inode_bitmap(&self, used: u32) -> Vec<u8> {
        let mut bitmap = alloc::vec![0u8; self.plan.block_size as usize];
        set_bits(&mut bitmap, 0, used);
        let per_group = self.plan.inodes_per_group;
        set_bits(&mut bitmap, per_group, self.plan.block_size * 8 - per_group);
        bitmap
    }

    /// The root directory (inode 2) and `lost+found` (inode 11), both in group 0.
    fn write_directories(&self, group0: &GroupLayout, lost_found: u32) -> Result<(), Ext2Error> {
        let bs = self.plan.block_size as usize;
        let root_block = group0.first_free;
        let first = root_block + 1;
        // The root: `.`, `..` and `lost+found`, the last record stretching to the end.
        let mut root = alloc::vec![0u8; bs];
        put_dirent(&mut root, 0, ROOT_INO, 12, b".");
        put_dirent(&mut root, 12, ROOT_INO, 12, b"..");
        put_dirent(&mut root, 24, DEFAULT_FIRST_INO, bs - 24, b"lost+found");
        self.put_block(root_block, &root)?;
        self.put_inode(group0, ROOT_INO, 0o755, 3, &[root_block])?;

        // `lost+found`: `.`/`..` first, then empty whole-block records fsck can fill.
        let mut head = alloc::vec![0u8; bs];
        put_dirent(&mut head, 0, DEFAULT_FIRST_INO, 12, b".");
        put_dirent(&mut head, 12, ROOT_INO, bs - 12, b"..");
        self.put_block(first, &head)?;
        let mut empty = alloc::vec![0u8; bs];
        put16(&mut empty, DE_REC_LEN, bs as u16);
        for block in first + 1..first + lost_found {
            self.put_block(block, &empty)?;
        }
        let blocks: Vec<u32> = (first..first + lost_found).collect();
        self.put_inode(group0, DEFAULT_FIRST_INO, 0o700, 2, &blocks)
    }

    /// Write directory inode `ino` (root owned) whose data lives in direct `blocks`.
    fn put_inode(
        &self,
        group0: &GroupLayout,
        ino: u32,
        mode: u16,
        links: u16,
        blocks: &[u32],
    ) -> Result<(), Ext2Error> {
        let bytes = blocks.len() as u32 * self.plan.block_size;
        let mut inode = [0u8; INODE_CORE_SIZE];
        put16(&mut inode, INO_MODE, S_IFDIR | mode);
        put32(&mut inode, INO_SIZE, bytes);
        for field in [INO_ATIME, INO_CTIME, INO_MTIME] {
            put32(&mut inode, field, self.stamp);
        }
        put16(&mut inode, INO_LINKS, links);
        put32(&mut inode, INO_BLOCKS, bytes / SECTOR_SIZE as u32);
        for (slot, block) in blocks.iter().enumerate() {
            put32(&mut inode, INO_BLOCK + slot * 4, *block);
        }
        let slot = u64::from(ino - 1) * INODE_CORE_SIZE as u64;
        self.put_bytes(u64::from(group0.inode_table) * self.bs() + slot, &inode)
    }

    fn put_block(&self, block: u32, data: &[u8]) -> Result<(), Ext2Error> {
        self.put_bytes(u64::from(block) * self.bs(), data)
    }

    /// Write `data` at byte `offset`. A range that does not start and end on a
    /// sector boundary has its edge sectors read back and patched.
    fn put_bytes(&self, offset: u64, data: &[u8]) -> Result<(), Ext2Error> {
        let sector = SECTOR_SIZE as u64;
        let first = offset / sector;
        let last = (offset + data.len() as u64).div_ceil(sector);
        let mut buf = alloc::vec![0u8; ((last - first) * sector) as usize];
        if !offset.is_multiple_of(sector) || !(data.len() as u64).is_multiple_of(sector) {
            self.io.read_sectors(first, &mut buf).map_err(io_error)?;
        }
        let at = (offset - first * sector) as usize;
        buf[at..at + data.len()].copy_from_slice(data);
        self.io.write_sectors(first, &buf).map_err(io_error)
    }

    /// Zero `count` blocks from `block` (an inode table must start empty).
    fn zero_blocks(&self, block: u32, count: u32) -> Result<(), Ext2Error> {
        let zeros = alloc::vec![0u8; (ZERO_RUN * self.bs()) as usize];
        let mut next = u64::from(block);
        let end = next + u64::from(count);
        while next < end {
            let run = ZERO_RUN.min(end - next);
            self.put_bytes(next * self.bs(), &zeros[..(run * self.bs()) as usize])?;
            next += run;
        }
        Ok(())
    }
}

/// Write a directory entry of `rec_len` bytes at `offset` of `block`.
fn put_dirent(block: &mut [u8], offset: usize, ino: u32, rec_len: usize, name: &[u8]) {
    put32(block, offset + DE_INO, ino);
    put16(block, offset + DE_REC_LEN, rec_len as u16);
    block[offset + DE_NAME_LEN] = name.len() as u8;
    block[offset + DE_FILE_TYPE] = FT_DIRECTORY;
    block[offset + DE_HEADER..offset + DE_HEADER + name.len()].copy_from_slice(name);
}

/// Set `count` bits from bit index `start` (LSB first, as ext2 does).
fn set_bits(bitmap: &mut [u8], start: u32, count: u32) {
    for bit in start..start + count {
        bitmap[(bit / 8) as usize] |= 1 << (bit % 8);
    }
}
