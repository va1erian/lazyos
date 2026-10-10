//! The requester's half of a [`UserDisk`]: one request through the slot,
//! waiting for the provider in slices, and the [`BlockDevice`] surface the
//! filesystems use (docs/architecture/usb-storage.md).

use core::sync::atomic::{AtomicU32, Ordering};

use super::{
    now, park, test_clock, Op, Phase, Request, State, UserDisk, DEAD_AFTER_TIMEOUTS,
    MAX_REQUEST_BYTES, NAMES, QUEUE_TICKS, SLICE_TICKS, SLOT_TICKS, TAKEN_TICKS,
};
use crate::block::{BlockDevice, BlockError, SECTOR_SIZE};
use crate::task::{self, wait::WaitQueue};

impl UserDisk {
    /// One request through the slot, start to finish.
    fn transact(
        &self,
        op: Op,
        lba: u64,
        read: Option<&mut [u8]>,
        write: Option<&[u8]>,
    ) -> Result<(), BlockError> {
        if !task::relax::can_block() {
            report_unparkable();
            return Err(BlockError::Io);
        }
        let slot_deadline = now() + SLOT_TICKS;
        let bytes = read.as_ref().map_or(0, |b| b.len()) + write.map_or(0, |b| b.len());
        // Take the request slot.
        let tag = loop {
            {
                let mut state = self.state.lock();
                if !state.alive {
                    return Err(BlockError::Io);
                }
                if !state.busy {
                    state.busy = true;
                    state.next_tag += 1;
                    let tag = state.next_tag;
                    state.request = Request {
                        tag,
                        op,
                        lba,
                        bytes,
                    };
                    if let Some(data) = write {
                        state.bounce[..data.len()].copy_from_slice(data);
                    }
                    state.phase = Phase::Queued;
                    state.stats.requests += 1;
                    break tag;
                }
            }
            if now() >= slot_deadline {
                return Err(BlockError::Io);
            }
            self.wait(&self.idle, slot_deadline);
        };
        let queued_at = now();
        self.work.notify_all();
        let result = self.await_completion(tag, queued_at, read);
        self.idle.notify_one();
        if result.is_err() {
            self.state.lock().stats.errors += 1;
        }
        result
    }

    /// Wait for request `tag` (queued at `queued_at`), then release the slot.
    fn await_completion(
        &self,
        tag: u64,
        mut queued_at: u64,
        read: Option<&mut [u8]>,
    ) -> Result<(), BlockError> {
        // Whether an untaken request's deadline may still be pushed back
        // because its provider is busy with another of its disks, and until
        // when: one taken request's worth, so with its own queue and taken
        // time a holder stays within `SLOT_TICKS`.
        let mut may_defer = true;
        let defer_until = queued_at + TAKEN_TICKS;
        loop {
            let deadline;
            let owner;
            {
                let mut state = self.state.lock();
                // Until the provider takes the request it only has to look;
                // once taken, the device gets its time.
                let taken = state.phase == Phase::Taken;
                deadline = if taken {
                    state.taken_at + TAKEN_TICKS
                } else {
                    queued_at + QUEUE_TICKS
                };
                owner = state.owner;
                debug_assert_eq!(state.request.tag, tag);
                if let Phase::Done(result) = state.phase {
                    if let (Ok(()), Some(buf)) = (result, read) {
                        buf.copy_from_slice(&state.bounce[..buf.len()]);
                    }
                    state.timeouts = 0;
                    release(&mut state);
                    return result;
                }
                if !state.alive || !task::live(state.owner) {
                    state.alive = false;
                    release(&mut state);
                    return Err(BlockError::Io);
                }
                if now() >= deadline && (taken || !may_defer) {
                    self.time_out(&mut state, taken);
                    return Err(BlockError::Io);
                }
            }
            if now() >= deadline {
                // Untaken: a provider serving another of its sticks (one
                // transfer at a time) has not neglected this one. Checked
                // with this disk's lock dropped: disks are locked one at a time.
                if now() < defer_until && super::busy_elsewhere(self.index, owner, now()) {
                    queued_at = now();
                } else {
                    may_defer = false;
                }
                continue;
            }
            self.wait(&self.done, (now() + SLICE_TICKS).min(deadline));
        }
    }

    /// The request in flight timed out (`taken`: in the provider): count
    /// it, report it, kill the disk after too many in a row, free the slot.
    fn time_out(&self, state: &mut State, taken: bool) {
        state.timeouts += 1;
        state.stats.timeouts += 1;
        let request = state.request;
        serial_println!(
            "block: {}: {:?} lba {} ({} bytes) timed out {} after {} s",
            NAMES[self.index],
            request.op,
            request.lba,
            request.bytes,
            if taken { "in the provider" } else { "untaken" },
            if taken { TAKEN_TICKS } else { QUEUE_TICKS } / 100
        );
        if state.timeouts >= DEAD_AFTER_TIMEOUTS {
            state.alive = false;
            serial_println!(
                "block: {}: provider stopped answering; disk dead",
                NAMES[self.index]
            );
        }
        release(state);
    }

    /// Park until `deadline`, or let the test's fake provider run instead.
    fn wait(&self, queue: &WaitQueue, deadline: u64) {
        if test_clock::serve(self.index) {
            return;
        }
        park(queue, deadline);
    }
}

/// Free the request slot: a late completion of this tag is now stale.
fn release(state: &mut State) {
    state.phase = Phase::Idle;
    state.busy = false;
}

/// A request from a context that holds the task table or is too deep in its
/// kernel stack to park: say so once.
fn report_unparkable() {
    static SEEN: AtomicU32 = AtomicU32::new(0);
    if SEEN.fetch_add(1, Ordering::Relaxed) == 0 {
        serial_println!("block: provider I/O from a context that cannot sleep; failed");
    }
}

impl BlockDevice for UserDisk {
    fn name(&self) -> &'static str {
        NAMES[self.index]
    }

    fn sector_count(&self) -> u64 {
        self.state.lock().sectors
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        self.check_range(lba, buf.len())?;
        for (index, chunk) in buf.chunks_mut(MAX_REQUEST_BYTES).enumerate() {
            let at = lba + (index * MAX_REQUEST_BYTES / SECTOR_SIZE) as u64;
            self.transact(Op::Read, at, Some(chunk), None)?;
        }
        Ok(())
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        self.check_range(lba, buf.len())?;
        if !self.is_writable() {
            return Err(BlockError::ReadOnly);
        }
        for (index, chunk) in buf.chunks(MAX_REQUEST_BYTES).enumerate() {
            let at = lba + (index * MAX_REQUEST_BYTES / SECTOR_SIZE) as u64;
            self.transact(Op::Write, at, None, Some(chunk))?;
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), BlockError> {
        self.transact(Op::Flush, 0, None, None)
    }

    fn is_writable(&self) -> bool {
        let state = self.state.lock();
        state.alive && state.writable
    }
}
