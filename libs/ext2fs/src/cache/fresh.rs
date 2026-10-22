//! Inodes no writeback has written since they were allocated.
//!
//! The disk's copy of such an inode's table slot is dead or never used: no
//! link count in it that a second name could outrun. Every inode freed
//! leaves a dead slot (an `unlink` writes no links and a deletion time, a
//! failed `create` or `mkdir` does the same through `Ext2::abandon_inode`)
//! or the slot it had at `format`. `Ext2::link` uses that to skip its
//! barrier: the barrier keeps a second name from reaching the disk ahead of
//! the raised link count, and a file whose own inode is not on the disk yet
//! has no count there to be too low. The new name then stands where the
//! first one already did, in the documented "entry naming an inode whose
//! initialisation was lost" class (`docs/architecture/block-cache.md`),
//! instead of costing a writeback of the file's data and a device flush
//! inside the volume lock (a cache that makes a file and links it a moment
//! later, WebKit's blob storage, did that once per body).
//!
//! The set only ever errs towards *not fresh*: every path that writes an
//! inode-table block removes the inodes in it, before the request is issued
//! (a failed or half-done write still counts), a snapshot taken for a
//! piecewise commit forgets all of them (its pieces write an older version of
//! blocks, at times of their own), and a journaled volume keeps none (its
//! barrier is a no-op).

use alloc::vec::Vec;

use super::BlockCache;

impl BlockCache {
    /// Note that regular file inode `ino` was just allocated: its table
    /// entry has never been written.
    pub fn mark_inode_fresh(&mut self, ino: u32) {
        if self.journal.is_none() {
            self.fresh_inodes.insert(ino);
        }
    }

    /// Whether no writeback has written inode `ino` since
    /// [`BlockCache::mark_inode_fresh`].
    pub fn inode_is_fresh(&self, ino: u32) -> bool {
        self.fresh_inodes.contains(&ino)
    }

    /// Inode-table block `block` is about to be written whole: every inode
    /// in it may reach the disk.
    pub(super) fn inodes_written(&mut self, block: u64) {
        if self.fresh_inodes.is_empty() {
            return;
        }
        if let Some(range) = self.roles.inodes_of(block) {
            // The inodes of one block, not a walk of the whole set: a
            // writeback runs this for every table block between two pauses.
            let inside: Vec<u32> = self.fresh_inodes.range(range).copied().collect();
            for ino in inside {
                self.fresh_inodes.remove(&ino);
            }
        }
    }

    /// The table entries of `wanted` are about to be written into their
    /// blocks (the rest of those blocks stay as the disk has them).
    pub(super) fn inodes_patched(&mut self, wanted: &[u32]) {
        for ino in wanted {
            self.fresh_inodes.remove(ino);
        }
    }
}
