//! Which writeback phase a block belongs to.
//!
//! ext2 has no journal, so the only crash protection a write-back cache can
//! keep is the *order* in which a writeback reaches the disk. Every writeback
//! writes all dirty blocks in five phases (`docs/architecture/block-cache.md`
//! has the argument in full):
//!
//! 1. [`Phase::Fresh`]: blocks allocated since they were last written back.
//!    Nothing durable points at them yet, so writing them first is always
//!    safe, and it means no pointer can ever reach the disk ahead of the
//!    bytes it points to (no stale data from a block's previous owner).
//! 2. [`Phase::Alloc`]: the group descriptor table and the bitmaps, so a block
//!    or inode is marked used before anything durable references it.
//! 3. [`Phase::Inodes`]: the inode tables, so a directory entry never reaches
//!    the disk ahead of the inode it names.
//! 4. [`Phase::Content`]: every other block: data overwritten in place, and
//!    the indirect and directory blocks that were already linked.
//! 5. [`Phase::Super`]: the superblock (free counters and `s_state`) last.
//!
//! Frees go the other way (pointer gone before the bitmap bit clears); the
//! volume defers them to the end of a commit instead (`commit.rs`). The roles
//! of the metadata blocks never move after `format`, so they are read once at
//! mount.

use alloc::vec::Vec;
use core::ops::Range;

/// Writeback phases, in the order they reach the disk.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Phase {
    Fresh,
    Alloc,
    Inodes,
    Content,
    Super,
}

/// The fixed metadata locations of one volume.
#[derive(Default)]
pub struct Roles {
    /// The block holding the superblock.
    pub super_block: u64,
    /// The group descriptor table.
    pub gdt: Range<u64>,
    /// Block and inode bitmaps, sorted.
    pub bitmaps: Vec<u64>,
    /// Inode tables, sorted by start.
    pub inode_tables: Vec<Range<u64>>,
}

impl Roles {
    /// The phase of `block`; `fresh` says it was allocated and not yet
    /// written back.
    pub fn phase(&self, block: u64, fresh: bool) -> Phase {
        if block == self.super_block {
            Phase::Super
        } else if self.gdt.contains(&block) || self.bitmaps.binary_search(&block).is_ok() {
            Phase::Alloc
        } else if self.in_inode_table(block) {
            Phase::Inodes
        } else if fresh {
            Phase::Fresh
        } else {
            Phase::Content
        }
    }

    fn in_inode_table(&self, block: u64) -> bool {
        // The last table starting at or before `block` is the only candidate.
        let after = self
            .inode_tables
            .partition_point(|table| table.start <= block);
        after > 0 && self.inode_tables[after - 1].contains(&block)
    }
}
