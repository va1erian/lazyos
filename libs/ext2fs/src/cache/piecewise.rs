//! Writeback in pieces: a commit that lets go of the volume between bounded
//! stretches of writes (issue #940).
//!
//! One [`BlockCache::flush`] writes everything at once, and on a slow disk (a
//! USB stick behind a user-space driver) the volume is held for all of it:
//! 685 blocks took 1.37 s on a real PC, and every reader of `/home` waited.
//! A piecewise writeback takes a *snapshot* of the dirty set
//! ([`BlockCache::begin_snapshot`]), then writes it in phase order a few
//! requests at a time ([`BlockCache::flush_piece`]); the host releases the
//! volume between calls, and other operations run meanwhile.
//!
//! Those operations may change blocks the snapshot has not written yet, and
//! writing the *new* version early could put a pointer on the disk ahead of
//! what it points to (an inode block that names a block allocated after the
//! snapshot, which is not in it and so not written yet). So a block changed
//! while its piece is pending keeps the snapshot's version aside (its
//! [`Shadow`]); the piece writes that, and the block stays dirty with the new
//! version for the next writeback. The disk therefore only ever receives a
//! prefix of one consistent image in the crash-safe phase order, exactly as
//! with a single flush. A block written by anyone else meanwhile (a full
//! flush, `fsync`) leaves the snapshot: the disk has a newer version.
//!
//! The phase of each member is fixed when the snapshot is taken, and a block
//! keeps its `fresh` flag until its own piece is written, so a full flush
//! that overtakes the snapshot still writes it in the right phase. A
//! journaled volume commits atomically already and takes no snapshot.

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::roles::Phase;
use super::{page, BlockCache, Slot};
use crate::{BlockIo, IoError};

/// A snapshot member's content from before it was changed again.
pub(super) struct Shadow {
    pub bytes: Box<[u8]>,
    /// Whether a later change allocated the block anew (it is fresh again
    /// once the snapshot's version is on the disk).
    late_fresh: bool,
}

/// The members of the writeback in progress, sorted by (phase, block).
pub(super) struct Snapshot {
    order: Vec<(Phase, u64, usize)>,
    next: usize,
}

/// Called by `write` before it replaces `slot`'s content: keep the version
/// a pending snapshot piece must still write.
pub(super) fn keep_snapshot_version(slot: &mut Slot, block_size: usize, fresh: bool) {
    if !slot.snap {
        return;
    }
    match &mut slot.shadow {
        Some(shadow) => shadow.late_fresh |= fresh,
        None => {
            slot.shadow = Some(Shadow {
                bytes: page(slot)[..block_size].into(),
                late_fresh: fresh,
            });
        }
    }
}

impl BlockCache {
    /// Start a piecewise writeback of everything dirty now (dropping one in
    /// progress). `false`: this cache cannot (a journal); use `flush`.
    pub fn begin_snapshot(&mut self, io: &dyn BlockIo) -> bool {
        if self.journal.is_some() {
            return false;
        }
        self.abort_snapshot();
        let order = self.order(io);
        for &(_, _, index) in &order {
            self.slots[index].snap = true;
        }
        self.stats.writebacks += 1;
        self.snapshot = Some(Snapshot { order, next: 0 });
        true
    }

    /// Write members of the snapshot, a run at a time, until `stop` (asked
    /// after each request) says enough. `Ok(true)`: the snapshot is done. On
    /// an error the snapshot stays; the caller aborts it.
    pub fn flush_piece(
        &mut self,
        io: &dyn BlockIo,
        stop: &mut dyn FnMut() -> bool,
    ) -> Result<bool, IoError> {
        let Some(mut snap) = self.snapshot.take() else {
            return Ok(true);
        };
        loop {
            while snap.next < snap.order.len() && !self.is_member(&snap.order[snap.next]) {
                snap.next += 1;
            }
            if snap.next >= snap.order.len() {
                return Ok(true);
            }
            io.pace();
            let end = self.member_run_end(&snap.order, snap.next);
            if let Err(error) = self.write_members(io, &snap.order[snap.next..end]) {
                self.snapshot = Some(snap);
                return Err(error);
            }
            snap.next = end;
            if stop() {
                self.snapshot = Some(snap);
                return Ok(false);
            }
        }
    }

    /// Give up the snapshot: its unwritten members stay dirty with their
    /// current content and join the next writeback.
    pub fn abort_snapshot(&mut self) {
        let Some(snap) = self.snapshot.take() else {
            return;
        };
        for &(_, block, index) in &snap.order[snap.next..] {
            if let Some(slot) = self.slots.get_mut(index) {
                if slot.block == Some(block) {
                    slot.snap = false;
                    slot.shadow = None;
                }
            }
        }
    }

    fn is_member(&self, &(_, block, index): &(Phase, u64, usize)) -> bool {
        self.slots
            .get(index)
            .is_some_and(|slot| slot.snap && slot.dirty && slot.block == Some(block))
    }

    /// One past the last entry of the run starting at `start`: members of
    /// one phase on consecutive blocks, at most `max_run` long.
    fn member_run_end(&self, order: &[(Phase, u64, usize)], start: usize) -> usize {
        let (phase, first, _) = order[start];
        let mut end = start + 1;
        while end < order.len()
            && end - start < self.max_run
            && order[end].0 == phase
            && order[end].1 == first + (end - start) as u64
            && self.is_member(&order[end])
        {
            end += 1;
        }
        end
    }

    /// Write one run of members as a request: the snapshot's version of each.
    fn write_members(
        &mut self,
        io: &dyn BlockIo,
        run: &[(Phase, u64, usize)],
    ) -> Result<(), IoError> {
        let size = self.block_size;
        let result = {
            let bufs: Vec<&[u8]> = run
                .iter()
                .map(|&(_, _, index)| {
                    let slot = &self.slots[index];
                    match &slot.shadow {
                        Some(shadow) => &shadow.bytes[..size],
                        None => &page(slot)[..size],
                    }
                })
                .collect();
            io.write_sectors_vectored(run[0].1 * self.sectors_per_block, &bufs)
        };
        if result.is_err() {
            self.failed = true;
        }
        result?;
        self.stats.write_requests += 1;
        self.stats.written += run.len() as u64;
        for &(_, _, index) in run {
            let slot = &mut self.slots[index];
            slot.snap = false;
            match slot.shadow.take() {
                // Changed since: still dirty, with the new content.
                Some(shadow) => slot.fresh = shadow.late_fresh,
                None => {
                    slot.dirty = false;
                    slot.fresh = false;
                    self.dirty -= 1;
                }
            }
        }
        Ok(())
    }
}
