//! Commits: the points where a cached volume's changes reach the disk, the
//! frees deferred until then, and how write-back errors are reported.
//!
//! A *commit* writes back every dirty block (in the phase order of
//! `cache/roles.rs`), then returns the blocks and inodes freed since the last
//! commit to their bitmaps, writes those back too, and flushes the device.
//!
//! Frees wait for the commit because a bitmap bit must clear only after the
//! pointer to the block is gone from the disk: the writeback puts bitmaps
//! *before* the blocks that reference them (right for allocation), so a free
//! landing in the same writeback could reach the disk ahead of its detach,
//! and a block both free and referenced is how two files end up sharing
//! data. Holding the frees also keeps a freed block from being reused (and
//! written with someone else's bytes) while a durable pointer still names it.
//! Freed space still counts as free in `statfs` right away; an allocation
//! that finds the disk full commits and tries once more.
//!
//! A failed writeback marks the volume errored: the next [`Ext2::flush`]
//! reports it (once, even if a retry then succeeded), and the state written at
//! the next clean stop carries `STATE_ERROR`, so the next mount says so.
//! Blocks that did not land stay dirty and are retried by every later commit;
//! nothing is dropped.

use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use super::*;
use crate::cache::Targets;

/// Pending frees past this many trigger a commit, bounding the list (a 2 GiB
/// file has half a million blocks).
const MAX_PENDING: usize = 16 * 1024;
/// Frees applied between two pause points ([`Ext2::set_pause`]).
const PAUSE_EVERY: usize = 256;

/// Blocks and inodes freed since the last commit, applied by it.
#[derive(Default)]
pub(crate) struct Pending {
    pub(super) blocks: Vec<u32>,
    /// Inode number and whether it was a directory.
    pub(super) inodes: Vec<(u32, bool)>,
}

impl Pending {
    pub(super) fn is_empty(&self) -> bool {
        self.blocks.is_empty() && self.inodes.is_empty()
    }
}

impl Ext2 {
    /// Free `block`, now or at the next commit (see the module docs).
    pub(super) fn free_block(&self, block: u32) -> Result<(), Ext2Error> {
        if !self.defer_frees {
            return self.release_block(block);
        }
        if self.read_only {
            return Err(Ext2Error::ReadOnly);
        }
        if block < self.first_data_block || block >= self.blocks_count {
            return Err(Ext2Error::Invalid);
        }
        let full = {
            let mut pending = self.pending.lock();
            pending.blocks.push(block);
            pending.blocks.len() >= MAX_PENDING
        };
        if full {
            self.commit_locked()?;
        }
        Ok(())
    }

    /// Free inode `ino`, now or at the next commit.
    pub(super) fn free_inode(&self, ino: u32, is_dir: bool) -> Result<(), Ext2Error> {
        if !self.defer_frees {
            return self.release_inode(ino, is_dir);
        }
        if self.read_only {
            return Err(Ext2Error::ReadOnly);
        }
        if ino < ROOT_INO || ino > self.inodes_count {
            return Err(Ext2Error::Invalid);
        }
        let full = {
            let mut pending = self.pending.lock();
            pending.inodes.push((ino, is_dir));
            pending.inodes.len() >= MAX_PENDING
        };
        if full {
            self.commit_locked()?;
        }
        Ok(())
    }

    /// Blocks and inodes freed but not yet back in the bitmaps; `statfs`
    /// counts them as free.
    pub(super) fn pending_frees(&self) -> (u32, u32) {
        let pending = self.pending.lock();
        (pending.blocks.len() as u32, pending.inodes.len() as u32)
    }

    pub(super) fn has_pending(&self) -> bool {
        !self.pending.lock().is_empty()
    }

    /// Commit with the volume lock held: write back, apply the deferred
    /// frees, write those back, flush the device. Does not touch `s_state`.
    pub(super) fn commit_locked(&self) -> Result<(), Ext2Error> {
        // Writes everything and applies every free: a piecewise commit in
        // progress has nothing left to do (`commit_piece.rs`).
        self.cancel_piecewise();
        // A journaled commit is atomic, so the detach and the free may share
        // one transaction: the first writeback exists only for plain ext2.
        let journaled = self.journal_active();
        if !journaled {
            self.write_back()?;
        }
        let pending = core::mem::take(&mut *self.pending.lock());
        let applied = if pending.is_empty() {
            Ok(())
        } else {
            self.apply(&pending)
        };
        if journaled || !pending.is_empty() {
            self.write_back()?;
        }
        applied?;
        self.io.flush().map_err(io_error)
    }

    /// Whether commits of this mount go through a journal.
    pub(super) fn journal_active(&self) -> bool {
        self.cache
            .as_ref()
            .is_some_and(|cache| cache.lock().has_journal())
    }

    /// Return every pending free to its bitmap. One bad entry (a double free
    /// means the image was already inconsistent) does not stop the rest.
    pub(super) fn apply(&self, pending: &Pending) -> Result<(), Ext2Error> {
        let mut first = Ok(());
        for (index, &block) in pending.blocks.iter().enumerate() {
            first = first.and(self.release_block(block));
            if index % PAUSE_EVERY == PAUSE_EVERY - 1 {
                self.pace();
            }
        }
        for &(ino, is_dir) in &pending.inodes {
            first = first.and(self.release_inode(ino, is_dir));
        }
        first
    }

    /// An ordering point inside one operation: everything changed so far is
    /// on the disk before anything changed after it.
    ///
    /// The phases order blocks by kind, which serves allocation and frees but
    /// not an operation whose crash safety needs two blocks *of the same kind*
    /// in a given order, or a directory block ahead of an inode: a rename must
    /// put the new name on the disk before the old one goes (else a crash can
    /// lose the file under both names), and the old name before the link count
    /// drops. Those operations call this between the steps; uncached, every
    /// write is already in order and this does nothing.
    ///
    /// A journaled volume needs none: the operation's changes are in the one
    /// transaction the next commit logs, which a crash replays whole or not
    /// at all, so every order this promises holds trivially (a rename is
    /// atomic, not merely "under at least one name").
    pub(super) fn barrier(&self) -> Result<(), Ext2Error> {
        let Some(cache) = &self.cache else {
            return Ok(());
        };
        if self.journal_active() {
            return Ok(());
        }
        // Inside an [`OpScope`] only the blocks the operation changed (and
        // what they depend on, `cache/targeted.rs`) need to land; outside
        // one, everything does.
        let tracked = cache.lock().is_tracking();
        if tracked {
            let (blocks, inodes) = cache.lock().take_touched();
            self.with_cache(cache, |cache, io| {
                cache.flush_targeted(
                    io,
                    &Targets {
                        owners: &[],
                        inodes: &inodes,
                        blocks: &blocks,
                        unknown: false,
                    },
                )
            })?;
        } else {
            self.write_back()?;
        }
        self.io.flush().map_err(io_error)
    }

    /// Whether no writeback has written inode `ino` since it was allocated:
    /// the disk holds nothing that names it (`cache/fresh.rs`). Never on an
    /// uncached volume, where every write is on the disk at once.
    pub(super) fn inode_is_fresh(&self, ino: u32) -> bool {
        self.cache
            .as_ref()
            .is_some_and(|cache| cache.lock().inode_is_fresh(ino))
    }

    /// Begin an operation whose [`Ext2::barrier`]s are narrow: the cache
    /// records the blocks written until the scope ends.
    pub(super) fn op_scope(&self) -> OpScope<'_> {
        if let Some(cache) = &self.cache {
            cache.lock().track_begin();
        }
        OpScope { volume: self }
    }

    /// Attribute the blocks written until the scope ends to inode `ino`, so
    /// [`Ext2::sync_file`] can find them.
    pub(super) fn owner_scope(&self, ino: u32) -> OwnerScope<'_> {
        let previous = self
            .cache
            .as_ref()
            .map_or(0, |cache| cache.lock().set_owner(ino));
        OwnerScope {
            volume: self,
            previous,
        }
    }

    /// Make one file durable: its data, its indirect tables, its inode and
    /// what they depend on, without writing what unrelated files left dirty
    /// or marking the volume clean. The caller holds the volume lock and has
    /// checked there is a cache without a journal. Without `flush` the
    /// device's cache flush is left to the caller (`Ext2::sync_file_written`).
    pub(super) fn sync_owner(&self, ino: u32, flush: bool) -> Result<(), Ext2Error> {
        let Some(cache) = &self.cache else {
            return Ok(());
        };
        self.with_cache(cache, |cache, io| {
            cache.flush_targeted(
                io,
                &Targets {
                    owners: &[ino],
                    inodes: &[ino],
                    blocks: &[],
                    unknown: true,
                },
            )
        })?;
        if flush {
            self.io.flush().map_err(io_error)?;
        }
        Ok(())
    }

    /// Write every dirty cached block back (nothing to do uncached).
    pub(super) fn write_back(&self) -> Result<(), Ext2Error> {
        match &self.cache {
            Some(cache) => self.with_cache(cache, |cache, io| cache.flush(io)),
            None => Ok(()),
        }
    }

    /// Record a failed write-back (see the module docs).
    pub(super) fn note_write_back_failure(&self) {
        self.errored.store(true, Ordering::Relaxed);
        self.error_unreported.store(true, Ordering::Relaxed);
    }

    /// `s_state` this mount stands for: the state found at mount, plus the
    /// error bit once a write-back has failed.
    pub(super) fn base_state(&self) -> u16 {
        if self.errored.load(Ordering::Relaxed) {
            self.mount_state | STATE_ERROR
        } else {
            self.mount_state
        }
    }

    /// The write-back error not yet reported to a `flush` caller, once.
    pub(super) fn take_unreported_error(&self) -> Result<(), Ext2Error> {
        if self.error_unreported.swap(false, Ordering::Relaxed) {
            Err(Ext2Error::Io)
        } else {
            Ok(())
        }
    }

    /// The periodic flusher's entry: commit whatever is dirty or pending,
    /// leaving the volume flagged dirty (only [`Ext2::flush`] marks it clean)
    /// and any error for the next `flush` to report.
    pub fn writeback(&self) -> Result<(), Ext2Error> {
        let _guard = self.lock.lock();
        if self.read_only || (self.dirty_blocks_locked() == 0 && !self.has_pending()) {
            return Ok(());
        }
        self.commit_locked()
    }

    /// Whether [`Ext2::writeback`] has anything to commit: dirty blocks, or
    /// frees still pending (a host that schedules writebacks itself asks).
    pub fn needs_writeback(&self) -> bool {
        let _guard = self.lock.lock();
        !self.read_only
            && (self.dirty_blocks_locked() > 0 || self.has_pending() || self.commit_in_progress())
    }

    /// Dirty blocks waiting in the cache (0 uncached).
    pub fn dirty_blocks(&self) -> usize {
        let _guard = self.lock.lock();
        self.dirty_blocks_locked()
    }

    pub(super) fn dirty_blocks_locked(&self) -> usize {
        self.cache
            .as_ref()
            .map_or(0, |cache| cache.lock().dirty_blocks())
    }

    /// The cache's counters, when the volume has one.
    pub fn cache_stats(&self) -> Option<CacheStats> {
        let _guard = self.lock.lock();
        self.cache.as_ref().map(|cache| cache.lock().stats())
    }

    /// Give every clean cached block's memory back (memory pressure); returns
    /// how many pages were released. Dirty blocks stay until a writeback.
    pub fn shrink_cache(&self) -> usize {
        let _guard = self.lock.lock();
        self.cache.as_ref().map_or(0, |cache| cache.lock().shrink())
    }
}

impl Drop for Ext2 {
    /// Dropping a volume commits what it still holds (an unmount without a
    /// sync loses nothing), but leaves `s_state` alone, as before the cache.
    fn drop(&mut self) {
        if self.cache.is_some() || self.has_pending() {
            let _ = self.writeback();
        }
    }
}

/// While alive, the cache records the blocks written, so a [`Ext2::barrier`]
/// writes those alone ([`Ext2::op_scope`]).
pub(crate) struct OpScope<'a> {
    volume: &'a Ext2,
}

impl Drop for OpScope<'_> {
    fn drop(&mut self) {
        if let Some(cache) = &self.volume.cache {
            cache.lock().track_end();
        }
    }
}

/// While alive, blocks written are attributed to one inode
/// ([`Ext2::owner_scope`]).
pub(crate) struct OwnerScope<'a> {
    volume: &'a Ext2,
    previous: u32,
}

impl Drop for OwnerScope<'_> {
    fn drop(&mut self) {
        if let Some(cache) = &self.volume.cache {
            cache.lock().set_owner(self.previous);
        }
    }
}
