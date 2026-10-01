//! Volume geometry for the formatter: how blocks, groups and inodes are laid out.
//!
//! Everything here follows from the requested size and block size alone, so the
//! encoder in `format.rs` never makes a layout decision. The rules match
//! `tools/mkdisk/geometry.py` and what [`Ext2::open`] accepts.

use super::*;

/// One inode per 16 KiB is mke2fs's "small" ratio and keeps the inode table
/// (and so the wasted space) small on a volume of tens or hundreds of megabytes.
const DEFAULT_BYTES_PER_INODE: u32 = 16 * 1024;
/// Smallest volume the formatter takes.
const MIN_BYTES: u64 = 1024 * 1024;
/// A trailing group smaller than its own metadata plus this many blocks is
/// dropped instead of formatted (mke2fs uses the same 50-block floor).
const MIN_GROUP_DATA_BLOCKS: u32 = 50;
/// mke2fs pre-allocates `lost+found` to 16 KiB, but never past the twelve direct
/// slots, which keeps the fresh inode free of indirection.
const LOST_FOUND_BYTES: u32 = 16 * 1024;

/// What the caller chooses about a new volume.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Geometry {
    /// 1024, 2048 or 4096.
    pub block_size: u32,
    /// Blocks to use at most. A trailing group too small to hold its own
    /// metadata is dropped, so the formatted volume may be a little smaller.
    pub blocks_count: u32,
    /// Inode density: one inode per this many bytes.
    pub bytes_per_inode: u32,
}

impl Geometry {
    /// The usual shape for `size_bytes` of device: 4 KiB blocks, one inode per
    /// 16 KiB.
    pub fn for_size(size_bytes: u64) -> Geometry {
        Geometry {
            block_size: 4096,
            blocks_count: u32::try_from(size_bytes / 4096).unwrap_or(u32::MAX),
            bytes_per_inode: DEFAULT_BYTES_PER_INODE,
        }
    }
}

/// Where one group keeps its metadata (absolute block numbers).
#[derive(Clone, Copy)]
pub(crate) struct GroupLayout {
    pub start: u32,
    /// Blocks in this group (the last may be short).
    pub blocks: u32,
    /// Whether the group carries a superblock and descriptor-table copy.
    pub has_backup: bool,
    pub block_bitmap: u32,
    pub inode_bitmap: u32,
    pub inode_table: u32,
    pub first_free: u32,
}

impl GroupLayout {
    /// Blocks from the group start up to its first data block.
    pub fn metadata_blocks(&self) -> u32 {
        self.first_free - self.start
    }
}

/// A validated [`Geometry`] with every derived figure filled in.
pub(crate) struct Plan {
    pub block_size: u32,
    pub blocks_count: u32,
    pub first_data_block: u32,
    pub blocks_per_group: u32,
    pub inodes_per_group: u32,
    pub groups: u32,
    pub gdt_blocks: u32,
    pub inode_table_blocks: u32,
}

impl Plan {
    /// Total inodes across all groups.
    pub fn inodes_count(&self) -> u32 {
        self.inodes_per_group * self.groups
    }

    /// Blocks pre-allocated to `lost+found` (2..=12).
    pub fn lost_found_blocks(&self) -> u32 {
        LOST_FOUND_BYTES
            .div_ceil(self.block_size)
            .clamp(2, DIRECT_BLOCKS)
    }

    /// The metadata placement of group `index`.
    pub fn group(&self, index: u32) -> GroupLayout {
        let start = self.first_data_block + index * self.blocks_per_group;
        let backup = has_backup(index);
        // The superblock and a descriptor-table copy sit at the group start.
        let cursor = start + if backup { 1 + self.gdt_blocks } else { 0 };
        GroupLayout {
            start,
            blocks: self.blocks_per_group.min(self.blocks_count - start),
            has_backup: backup,
            block_bitmap: cursor,
            inode_bitmap: cursor + 1,
            inode_table: cursor + 2,
            first_free: cursor + 2 + self.inode_table_blocks,
        }
    }
}

/// The sparse-super rule: groups 0, 1 and powers of 3, 5 and 7 carry backups.
pub(crate) fn has_backup(group: u32) -> bool {
    if group <= 1 {
        return true;
    }
    [3u64, 5, 7].into_iter().any(|base| {
        let mut power = base;
        while power < u64::from(group) {
            power *= base;
        }
        power == u64::from(group)
    })
}

/// Choose the layout for `geometry`, refusing sizes the format or the driver
/// cannot take.
pub(crate) fn plan(geometry: &Geometry) -> Result<Plan, Ext2Error> {
    let block_size = geometry.block_size;
    if ![1024, 2048, 4096].contains(&block_size) || geometry.bytes_per_inode == 0 {
        return Err(Ext2Error::Invalid);
    }
    if u64::from(geometry.blocks_count) * u64::from(block_size) < MIN_BYTES {
        return Err(Ext2Error::Invalid);
    }
    let first_data_block = u32::from(block_size == 1024);
    let mut blocks = geometry.blocks_count;
    loop {
        let groups = (blocks - first_data_block).div_ceil(block_size * 8);
        if groups > MAX_GROUPS {
            return Err(Ext2Error::Invalid); // the driver would refuse to mount it
        }
        let plan = with_groups(geometry, blocks, first_data_block, groups);
        let last = plan.group(groups - 1);
        if groups == 1 || last.blocks >= last.metadata_blocks() + MIN_GROUP_DATA_BLOCKS {
            return Ok(plan);
        }
        blocks -= last.blocks; // drop the runt group and re-plan
    }
}

/// Fill in the inode and descriptor-table sizes for a fixed group count.
fn with_groups(geometry: &Geometry, blocks: u32, first_data_block: u32, groups: u32) -> Plan {
    let block_size = geometry.block_size;
    let inodes_per_block = block_size / INODE_CORE_SIZE as u32;
    let wanted = u64::from(blocks) * u64::from(block_size) / u64::from(geometry.bytes_per_inode);
    let wanted = wanted.max(u64::from(DEFAULT_FIRST_INO)) as u32;
    let per_group = wanted.div_ceil(groups);
    // Whole inode-table blocks per group; the driver rejects a ragged tail.
    let per_group = per_group.div_ceil(inodes_per_block) * inodes_per_block;
    let per_group = per_group.min(block_size * 8); // one inode-bitmap block
    Plan {
        block_size,
        blocks_count: blocks,
        first_data_block,
        blocks_per_group: block_size * 8,
        inodes_per_group: per_group,
        groups,
        gdt_blocks: (groups * GD_SIZE as u32).div_ceil(block_size),
        inode_table_blocks: per_group / inodes_per_block,
    }
}
