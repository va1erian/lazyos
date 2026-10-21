//! A commit in pieces (issue #940): the same two writebacks as
//! [`Ext2::commit_locked`], but the host gets the volume back between
//! bounded stretches of writes.
//!
//! 1. Snapshot the dirty set ([`BlockCache::begin_snapshot`]) and note how
//!    many frees are pending; write the snapshot piece by piece. Every detach
//!    the noted frees wait for is in it.
//! 2. When it is all written, return the noted frees to their bitmaps (frees
//!    queued since wait for the next commit: their detaches are not in the
//!    snapshot), snapshot again, and write that piece by piece. With no
//!    frees noted there is no second stage, as in a plain commit.
//!
//! Between pieces other operations run. A block they change while its piece
//! is pending is written as the snapshot saw it (`cache/piecewise.rs`), so
//! the disk only ever gets a prefix of a consistent image in phase order, as
//! with one flush. Any full commit ([`Ext2::commit_locked`]: `sync`, a full
//! disk, unmount) ends a piecewise one: it writes everything and applies
//! every pending free anyway.
//!
//! The device's cache flush is not part of the last piece: the host calls
//! [`Ext2::flush_device`] once it has let go of the volume.

use super::*;

/// How far a piecewise commit has come.
pub(crate) struct Piecewise {
    /// In the second writeback (the noted frees are applied).
    second: bool,
    /// Pending frees the second stage applies: a prefix of the lists.
    blocks: usize,
    inodes: usize,
    /// The first error applying frees, reported when the commit ends.
    applied: Result<(), Ext2Error>,
}

/// What [`Ext2::writeback_piece`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Progress {
    /// Call again (after letting go of the volume).
    More,
    /// Everything is written; the device's cache flush is still owed
    /// ([`Ext2::flush_device`]).
    Done,
}

impl Ext2 {
    /// Write the next piece of a commit of the dirty blocks and pending frees,
    /// starting one if none is in progress: requests until `stop` (asked after
    /// each) says enough. A volume that cannot be written in pieces (no cache,
    /// a journal) commits whole. An error ends the commit; blocks that did not
    /// land stay dirty for the next one.
    pub fn writeback_piece(&self, stop: &mut dyn FnMut() -> bool) -> Result<Progress, Ext2Error> {
        let _guard = self.lock.lock();
        if self.read_only {
            return Ok(Progress::Done);
        }
        let mut state = self.piecewise.lock();
        if state.is_none() {
            if self.dirty_blocks_locked() == 0 && !self.has_pending() {
                return Ok(Progress::Done);
            }
            match self.begin_piecewise() {
                Some(started) => *state = Some(started),
                None => {
                    drop(state);
                    self.commit_locked()?;
                    return Ok(Progress::Done);
                }
            }
        }
        let Some(cache) = &self.cache else {
            *state = None;
            return Ok(Progress::Done);
        };
        let written = self.with_cache(cache, |cache, io| cache.flush_piece(io, stop));
        match written {
            Err(error) => {
                cache.lock().abort_snapshot();
                *state = None;
                Err(error)
            }
            Ok(false) => Ok(Progress::More),
            Ok(true) => self.next_stage(&mut state),
        }
    }

    /// Whether a piecewise commit is between pieces.
    pub fn commit_in_progress(&self) -> bool {
        self.piecewise.lock().is_some()
    }

    /// The device's cache flush, for the host to make after the last piece
    /// without holding the volume.
    pub fn flush_device(&self) -> Result<(), Ext2Error> {
        if self.read_only {
            return Ok(());
        }
        self.io.flush().map_err(io_error)
    }

    /// Snapshot the dirty set and note the pending frees; `None` when the
    /// volume cannot commit in pieces.
    fn begin_piecewise(&self) -> Option<Piecewise> {
        let cache = self.cache.as_ref()?;
        let took = self
            .with_cache(cache, |cache, io| Ok(cache.begin_snapshot(io)))
            .ok()?;
        if !took {
            return None;
        }
        let pending = self.pending.lock();
        Some(Piecewise {
            second: false,
            blocks: pending.blocks.len(),
            inodes: pending.inodes.len(),
            applied: Ok(()),
        })
    }

    /// The snapshot is written: apply the noted frees and snapshot again, or
    /// finish.
    fn next_stage(&self, state: &mut Option<Piecewise>) -> Result<Progress, Ext2Error> {
        let Some(mut piecewise) = state.take() else {
            return Ok(Progress::Done);
        };
        if piecewise.second {
            return piecewise.applied.map(|()| Progress::Done);
        }
        let taken = {
            let mut pending = self.pending.lock();
            commit::Pending {
                blocks: pending.blocks.drain(..piecewise.blocks).collect(),
                inodes: pending.inodes.drain(..piecewise.inodes).collect(),
            }
        };
        if taken.is_empty() {
            // Nothing to free: the one writeback was the whole commit.
            return Ok(Progress::Done);
        }
        piecewise.applied = self.apply(&taken);
        piecewise.second = true;
        let began = self.cache.as_ref().is_some_and(|cache| {
            self.with_cache(cache, |cache, io| Ok(cache.begin_snapshot(io)))
                .unwrap_or(false)
        });
        if !began {
            return piecewise.applied.map(|()| Progress::Done);
        }
        *state = Some(piecewise);
        Ok(Progress::More)
    }

    /// End a piecewise commit that a full one replaces (the lock is held).
    pub(super) fn cancel_piecewise(&self) {
        if self.piecewise.lock().take().is_some() {
            if let Some(cache) = &self.cache {
                cache.lock().abort_snapshot();
            }
        }
    }
}
