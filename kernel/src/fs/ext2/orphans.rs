//! Reclaiming orphaned `.unlinked-<n>` files after an unclean stop.
//!
//! `unlink` of an open file parks it under a hidden name in its directory
//! (see [`crate::fs::hidden`]) and deletes it at the last close. A stop in
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
use crate::fs::hidden;

/// Most directories one reclaim walk visits.
const MAX_SCAN_DIRS: usize = 4096;
/// Deepest directory nesting the walk enters (the root is depth 0).
const MAX_SCAN_DEPTH: usize = 32;

impl Ext2 {
    /// Delete every parked orphan and return how many were reclaimed.
    ///
    /// Does nothing on a read-only device or a volume that was cleanly
    /// unmounted. A file that cannot be reclaimed (an I/O error, a malformed
    /// entry) is reported and left for the next mount; it never fails the mount.
    pub fn reclaim_orphans(&self) -> usize {
        if self.read_only || self.was_clean_at_mount() {
            return 0;
        }
        let mut reclaimed = 0;
        let mut pending = alloc::vec![(String::from("/"), 0usize)];
        let mut visited = 0;
        while let Some((dir, depth)) = pending.pop() {
            if visited == MAX_SCAN_DIRS {
                crate::serial_println!("ext2: orphan scan stopped at {MAX_SCAN_DIRS} directories");
                break;
            }
            visited += 1;
            reclaimed += self.scan_dir(&dir, depth, &mut pending);
        }
        reclaimed
    }

    /// Reclaim the orphans directly inside `dir` and queue its subdirectories.
    fn scan_dir(&self, dir: &str, depth: usize, pending: &mut Vec<(String, usize)>) -> usize {
        let Ok(entries) = self.readdir(dir) else {
            return 0; // unreadable: leave it, the mount goes on
        };
        let mut reclaimed = 0;
        for entry in entries {
            let path = child_path(dir, &entry.name);
            let reserved = hidden::is_reserved(&entry.name);
            match entry.kind {
                FileKind::File if reserved => match self.unlink(&path) {
                    Ok(()) => reclaimed += 1,
                    Err(error) => {
                        crate::serial_println!("ext2: could not reclaim {path}: {error:?}");
                    }
                },
                // A reserved *directory* is not ours to look inside.
                FileKind::Dir if !reserved && depth < MAX_SCAN_DEPTH => {
                    pending.push((path, depth + 1));
                }
                _ => {}
            }
        }
        reclaimed
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
    ) -> Result<(), FsError> {
        // A crafted entry must not reach the reserved inodes (journal, resize).
        if ino < self.first_ino {
            return Err(FsError::Invalid);
        }
        if self.inode_allocated(ino)? {
            self.release_orphan_blocks(child)?;
            self.clear_orphan_inode(ino, child)?;
            self.free_inode(ino, false)?;
        }
        self.remove_entry(parent_ino, parent, name).map(|_| ())
    }

    /// Free every block `inode` reaches that is still allocated (step 1).
    fn release_orphan_blocks(&self, inode: &[u8; INODE_CORE_SIZE]) -> Result<(), FsError> {
        for slot in 0..BLOCK_SLOTS {
            let root = Self::direct_ptr(inode, slot);
            if root != 0 {
                self.release_tree(root, slot_depth(slot))?;
            }
        }
        Ok(())
    }

    /// Free the tree of pointer tables rooted at `block`, children before the
    /// table that names them, skipping blocks a previous run already freed.
    /// The recursion is at most [`MAX_DEPTH`] deep, so a cyclic pointer in a
    /// malformed image ends instead of looping.
    fn release_tree(&self, block: u32, depth: usize) -> Result<(), FsError> {
        if depth > 0 {
            for child in self.read_table(block)? {
                if child != 0 {
                    self.release_tree(child, depth - 1)?;
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
    ) -> Result<(), FsError> {
        put16(inode, INO_LINKS, 0);
        put32(inode, INO_DTIME, now());
        put32(inode, INO_SIZE, 0);
        put32(inode, INO_BLOCKS, 0);
        inode[INO_BLOCK..INO_BLOCK + BLOCK_SLOTS as usize * 4].fill(0);
        self.write_inode(ino, inode)
    }

    /// Whether the block bitmap marks `block` used.
    fn block_allocated(&self, block: u32) -> Result<bool, FsError> {
        if block < self.first_data_block || block >= self.blocks_count {
            return Err(FsError::Invalid);
        }
        let group = (block - self.first_data_block) / self.blocks_per_group;
        let index = (block - self.first_data_block) % self.blocks_per_group;
        let desc = self.read_group(group)?;
        let size = self.block_size as usize;
        let mut bitmap = [0u8; MAX_BLOCK_SIZE];
        self.read_block(u64::from(desc.block_bitmap), &mut bitmap[..size])?;
        Ok(Self::bitmap_test(&bitmap[..size], index))
    }

    /// Whether the inode bitmap marks `ino` used.
    fn inode_allocated(&self, ino: u32) -> Result<bool, FsError> {
        if ino == 0 || ino > self.inodes_count {
            return Err(FsError::Invalid);
        }
        let index = ino - 1;
        let desc = self.read_group(index / self.inodes_per_group)?;
        let size = self.block_size as usize;
        let mut bitmap = [0u8; MAX_BLOCK_SIZE];
        self.read_block(u64::from(desc.inode_bitmap), &mut bitmap[..size])?;
        Ok(Self::bitmap_test(
            &bitmap[..size],
            index % self.inodes_per_group,
        ))
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
