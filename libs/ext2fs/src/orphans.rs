//! Reclaiming orphaned `.unlinked-<n>` files after an unclean stop.
//!
//! `unlink` of an open file parks it under a hidden name in its directory
//! (a reserved name prefix the host owns) and deletes it at the last close. A stop in
//! between leaves the entry, its inode and its blocks behind. The hidden name
//! is this filesystem's orphan list: as long as any part of the file exists,
//! the name still leads to it, so a mount can always find and finish the job.
//!
//! # Deleting a parked file crash-safely
//!
//! A directory entry and an inode bitmap cannot be changed in one write, so the
//! order decides what a stop in the middle strands. An ordinary unlink drops
//! the name first, and truncate detaches blocks before freeing them: both leave
//! a leak nothing can find after a stop (safe, but not recoverable by name).
//! Deleting a *reserved* name instead keeps the name, and the pointers it leads
//! to, until the very end, so the delete can always be resumed:
//!
//! 1. free the blocks the inode reaches, deepest first (data, then the tables
//!    that named them);
//! 2. write the inode with no pointers;
//! 3. return the inode to the bitmap;
//! 4. remove the entry.
//!
//! A stop during step 1 leaves the inode pointing at some blocks that are
//! already free; a stop after step 3, a name for a *free* inode. Both are what
//! [`Ext2::discard_orphan`] accepts on the next run: every release skips what
//! is already free, and the inode is freed only if still allocated. That is
//! safe because nothing allocates between the stop and the retry (the retry is
//! the next mount, before the volume is visible). The price is a hazard for a
//! delete that *fails* (an I/O error) while the system keeps running: blocks
//! already freed could be reused before a retry frees them again. Such a device
//! is already failing writes everywhere, and the dirty flag stays set.
//!
//! # The scan
//!
//! Only a volume that was *not* flagged clean at mount is scanned (a clean one
//! pays nothing). The walk is bounded in directories and in depth so a
//! malformed or hostile image cannot stall the mount, and it never descends
//! into, follows, or deletes anything but a regular file whose name is
//! reserved.
//!
//! Known gap: an orphan parked and then left behind by a *clean* stop (a
//! `sync` after the unlink, then a power cut before any further write) sits on
//! a volume that says clean, so this scan skips it. It costs space, never
//! consistency, and the next unclean mount collects it.

use super::*;

/// Most directories one reclaim walk visits.
pub const MAX_SCAN_DIRS: usize = 4096;
/// Deepest directory nesting the walk enters (the root is depth 0).
const MAX_SCAN_DEPTH: usize = 32;
/// Most failures [`OrphanReport::failed`] records; the rest are dropped.
pub const MAX_FAILED: usize = 256;

/// What one [`Ext2::reclaim_orphans`] walk did, for the host to report (the
/// library has no log of its own).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct OrphanReport {
    /// Parked files deleted.
    pub reclaimed: usize,
    /// Parked files that could not be deleted, with why; left for the next
    /// mount. At most [`MAX_FAILED`] are listed.
    pub failed: Vec<(String, Ext2Error)>,
    /// Set when the walk hit [`MAX_SCAN_DIRS`] and stopped early.
    pub scan_truncated: bool,
}

impl Ext2 {
    /// Delete every parked orphan, the files whose name starts with
    /// `reserved_prefix` (the namespace the host reserves for parking).
    ///
    /// Does nothing on a read-only device or a volume that was cleanly
    /// unmounted. A file that cannot be reclaimed (an I/O error, a malformed
    /// entry) is reported and left for the next mount; it never fails the mount.
    pub fn reclaim_orphans(&self, reserved_prefix: &str) -> OrphanReport {
        let mut report = OrphanReport::default();
        // An empty prefix would name every file in the volume as an orphan.
        if reserved_prefix.is_empty() || self.read_only || self.was_clean_at_mount() {
            return report;
        }
        let mut pending = alloc::vec![(String::from("/"), 0usize)];
        let mut visited = 0;
        while let Some((dir, depth)) = pending.pop() {
            if visited == MAX_SCAN_DIRS {
                report.scan_truncated = true;
                break;
            }
            visited += 1;
            // Room left in the queue before the walk's directory budget is spent.
            let room = MAX_SCAN_DIRS.saturating_sub(visited + pending.len());
            self.scan_dir(
                &dir,
                depth,
                reserved_prefix,
                room,
                &mut pending,
                &mut report,
            );
        }
        report
    }

    /// Reclaim the orphans directly inside `dir` and queue its subdirectories.
    fn scan_dir(
        &self,
        dir: &str,
        depth: usize,
        prefix: &str,
        mut room: usize,
        pending: &mut Vec<(String, usize)>,
        report: &mut OrphanReport,
    ) {
        let Ok(entries) = self.readdir(dir) else {
            return; // unreadable: leave it, the mount goes on
        };
        for entry in entries {
            let path = child_path(dir, &entry.name);
            let reserved = entry.name.starts_with(prefix);
            match entry.kind {
                FileKind::File if reserved => match self.unlink_parked(&path) {
                    Ok(()) => report.reclaimed += 1,
                    Err(error) => {
                        if report.failed.len() < MAX_FAILED {
                            report.failed.push((path, error));
                        }
                    }
                },
                // A reserved *directory* is not ours to look inside.
                FileKind::Dir if !reserved && depth < MAX_SCAN_DEPTH => {
                    if room == 0 {
                        report.scan_truncated = true; // never queue past the budget
                    } else {
                        room -= 1;
                        pending.push((path, depth + 1));
                    }
                }
                _ => {}
            }
        }
    }

    /// Delete the parked file `name` (inode `ino`, already read as `child`)
    /// from directory `parent_ino`, in the resumable order described above.
    pub(super) fn discard_orphan(
        &self,
        parent_ino: u32,
        parent: &mut [u8; INODE_CORE_SIZE],
        name: &str,
        ino: u32,
        child: &mut [u8; INODE_CORE_SIZE],
    ) -> Result<(), Ext2Error> {
        // A crafted entry must not reach the reserved inodes (journal, resize).
        if ino < self.first_ino {
            return Err(Ext2Error::Invalid);
        }
        if self.inode_allocated(ino)? {
            self.release_orphan_blocks(child)?;
            self.clear_orphan_inode(ino, child)?;
            self.free_inode(ino, false)?;
        }
        self.remove_entry(parent_ino, parent, name).map(|_| ())
    }

    /// Free every block `inode` reaches that is still allocated (step 1).
    fn release_orphan_blocks(&self, inode: &[u8; INODE_CORE_SIZE]) -> Result<(), Ext2Error> {
        // One budget for the whole inode: a hostile image cannot make a
        // table visit more blocks than the volume has.
        let mut budget = self.blocks_count;
        for slot in 0..BLOCK_SLOTS {
            let root = Self::direct_ptr(inode, slot);
            if root != 0 {
                self.release_tree(root, slot_depth(slot), &mut budget)?;
            }
        }
        Ok(())
    }

    /// Free the tree of pointer tables rooted at `block`, children before the
    /// table that names them, skipping blocks a previous run already freed.
    /// The recursion is at most [`MAX_DEPTH`] deep, so a cyclic pointer in a
    /// malformed image ends instead of looping, and `budget` bounds the total
    /// number of blocks visited (a table whose entries all name one block
    /// would otherwise be walked a thousand times per level).
    fn release_tree(&self, block: u32, depth: usize, budget: &mut u32) -> Result<(), Ext2Error> {
        *budget = budget.checked_sub(1).ok_or(Ext2Error::Invalid)?;
        // Children are freed before the table that names them, so a table
        // that is already free has had its whole subtree released.
        if !self.block_allocated(block)? {
            return Ok(());
        }
        if depth > 0 {
            for child in self.read_table(block)? {
                if child != 0 {
                    self.release_tree(child, depth - 1, budget)?;
                }
            }
        }
        if self.block_allocated(block)? {
            self.free_block(block)?;
        }
        Ok(())
    }

    /// Persist `inode` with no links, size or block pointers (step 2), so the
    /// pointers to blocks just freed do not outlive them.
    fn clear_orphan_inode(
        &self,
        ino: u32,
        inode: &mut [u8; INODE_CORE_SIZE],
    ) -> Result<(), Ext2Error> {
        put16(inode, INO_LINKS, 0);
        put32(inode, INO_DTIME, self.now());
        put32(inode, INO_SIZE, 0);
        put32(inode, INO_BLOCKS, 0);
        inode[INO_BLOCK..INO_BLOCK + BLOCK_SLOTS as usize * 4].fill(0);
        self.write_inode(ino, inode)
    }

    /// Whether the block bitmap marks `block` used.
    fn block_allocated(&self, block: u32) -> Result<bool, Ext2Error> {
        if block < self.first_data_block || block >= self.blocks_count {
            return Err(Ext2Error::Invalid);
        }
        let group = (block - self.first_data_block) / self.blocks_per_group;
        let index = (block - self.first_data_block) % self.blocks_per_group;
        let desc = self.read_group(group)?;
        let size = self.block_size as usize;
        let mut bitmap = [0u8; MAX_BLOCK_SIZE];
        self.read_block(u64::from(desc.block_bitmap), &mut bitmap[..size])?;
        Self::bitmap_test(&bitmap[..size], index)
    }

    /// Whether the inode bitmap marks `ino` used.
    fn inode_allocated(&self, ino: u32) -> Result<bool, Ext2Error> {
        if ino == 0 || ino > self.inodes_count {
            return Err(Ext2Error::Invalid);
        }
        let index = ino - 1;
        let desc = self.read_group(index / self.inodes_per_group)?;
        let size = self.block_size as usize;
        let mut bitmap = [0u8; MAX_BLOCK_SIZE];
        self.read_block(u64::from(desc.inode_bitmap), &mut bitmap[..size])?;
        Self::bitmap_test(&bitmap[..size], index % self.inodes_per_group)
    }
}

/// Levels of pointer tables between inode slot `slot` and its data blocks.
fn slot_depth(slot: u32) -> usize {
    if slot < SINGLE_INDIRECT_SLOT {
        0
    } else {
        (slot - SINGLE_INDIRECT_SLOT) as usize + 1
    }
}

/// `name` inside directory `dir` (`/` or an absolute path without a trailing `/`).
fn child_path(dir: &str, name: &str) -> String {
    if dir == "/" {
        alloc::format!("/{name}")
    } else {
        alloc::format!("{dir}/{name}")
    }
}
