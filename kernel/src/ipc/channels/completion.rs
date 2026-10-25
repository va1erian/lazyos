//! A per-task completion queue for calls (issue #725;
//! docs/architecture/call-completion.md).
//!
//! A caller that does not park on `await_reply` (`OP_CALL_BEGIN` with no
//! immediate `OP_CALL_AWAIT`, as an event loop does) must still learn when
//! its transaction ended and why. The kernel records every terminal
//! transition in the *caller's* queue here — reply, deadline, cancel, peer
//! death, a served poll — and the `wait` op answers a `WAIT_CALLS` doorbell
//! with [`CALLS_READY`](ready) while the queue is non-empty, so one named
//! item covers every call in flight. `OP_CALL_DRAIN` takes the entries; the
//! transaction itself stays in the registry in its terminal state until the
//! caller's own `await` (which a completion is not: it only says "ended")
//! consumes the reply, exactly as a `WAIT_ITEM_CALL` wait does.
//!
//! The queue is bounded: past [`CAPACITY`] entries the oldest is dropped. A
//! dropped entry only costs a wake the doorbell no longer reports — the
//! caller's own await (or a later drain of the registry through
//! `await_reply`) still finds the terminal state — and [`MAX_OUTSTANDING`]
//! per channel plus one caller's in-flight calls keep that far away in
//! practice. Recordings from every hook run outside the `CHANNELS` lock,
//! with this module's own lock never held across a park.

use alloc::collections::VecDeque;

use spin::Mutex;

use super::*;

/// The earlier of two optional nanosecond deadlines (`None` is "never").
/// A copy of [`waitcall::earlier`] in ticks, because that one is `pub(super)`
/// to the wait pieces and this module records completions from anywhere.
fn earlier_ns(earliest: Option<u64>, deadline: Option<u64>) -> Option<u64> {
    match (earliest, deadline) {
        (Some(earliest), Some(deadline)) => Some(earliest.min(deadline)),
        (earliest, None) => earliest,
        (None, deadline) => deadline,
    }
}

/// Finished transactions kept for one task before the oldest is dropped.
pub const CAPACITY: usize = 64;

/// The entries of every task slot, in completion order.
static QUEUES: [Mutex<VecDeque<u64>>; task::MAX_TASKS] =
    [const { Mutex::new(VecDeque::new()) }; task::MAX_TASKS];

/// The queue of `slot`, or `None` for a slot no task owns (the scalar
/// callers all come from the task table, so it would be a table bug);
/// answering with another slot's queue would poison it.
fn queue(slot: usize) -> Option<&'static Mutex<VecDeque<u64>>> {
    QUEUES.get(slot)
}

/// Record that `caller`'s call `txn_id` ended (any terminal state): one
/// entry, and a wake for the slot when it is parked in `wait`. The caller
/// may also be running (its own cancel, or the deadline it noticed in
/// `await_reply`); `notify_task` wakes only a parked task, so nothing is
/// lost. Never takes the `CHANNELS` lock: call it once the registry lock is
/// released on each terminal-transition path.
pub fn record(caller: usize, txn_id: u64) {
    {
        let Some(queue) = queue(caller) else {
            return;
        };
        let mut entries = queue.lock();
        if entries.len() >= CAPACITY {
            entries.pop_front();
        }
        if entries.back() == Some(&txn_id) {
            // The same transaction twice in a row (a rescan recorded a
            // terminal one again) is one entry.
            return;
        }
        entries.push_back(txn_id);
    }
    // The level check is what a parked loop resumes on; the wake is only
    // the shortcut. Locked separately: nothing under an ever-held lock.
    wake_parked(caller);
}

/// Whether `me`'s queue holds anything (the wait's doorbell: level, like
/// `WAIT_CHILD`).
pub fn ready(me: usize) -> bool {
    queue(me).is_some_and(|queue| !queue.lock().is_empty())
}

/// The earliest deadline of `me`'s still-pending calls, `None` when none is
/// open. `CALLS_READY` waiters park until it exactly as `WAIT_ITEM_CALL`
/// parkers do on their named calls: the doorbell covers every call, and a
/// call whose caller never parks on it (`await_reply`) still has to end at
/// its own deadline.
pub(super) fn caller_deadline(me: usize) -> Option<u64> {
    let channels = CHANNELS.lock();
    let mut earliest = None;
    for channel in channels.iter() {
        for txn in &channel.txns {
            if txn.caller == me && txn.state == TxnState::Pending {
                earliest = earlier_ns(earliest, txn.deadline);
            }
        }
    }
    earliest
}

/// Expire every call of `me` whose deadline is due (after a timed-out
/// park), exactly as the named `WAIT_ITEM_CALL` expiry does — but for the
/// calls the `WAIT_CALLS` doorbell covers without being named — and record
/// each end in `me`'s own queue. A reply raced the deadline and won: then
/// the transaction is already terminal and expired a no-op.
pub(super) fn expire_due(me: usize) {
    let mut ended: Vec<u64> = Vec::new();
    {
        let mut channels = CHANNELS.lock();
        for channel in channels.iter_mut() {
            for index in 0..channel.txns.len() {
                let txn = &channel.txns[index];
                if txn.caller == me
                    && txn.state == TxnState::Pending
                    && txn
                        .deadline
                        .is_some_and(|deadline| deadline <= task::ticks())
                {
                    let (poll, id) = (txn.poll && !txn.served, txn.id);
                    time_out(channel, index, poll);
                    ended.push(id);
                }
            }
        }
    }
    for id in ended {
        record(me, id);
    }
}

/// Take up to `out.len()` finished transactions of `me`, oldest first; the
/// entries are consumed.
pub fn take(me: usize, out: &mut [u64]) -> usize {
    let Some(queue) = queue(me) else {
        return 0;
    };
    let mut entries = queue.lock();
    let count = out.len().min(entries.len());
    for taken in out.iter_mut().take(count) {
        *taken = entries.pop_front().unwrap_or_default();
    }
    count
}

/// Forget everything a task slot still holds: `forget_task` calls it, so a
/// reused slot never inherits a dead task's entries (they would wake it for
/// transactions that are not its own).
pub fn forget(slot: usize) {
    if let Some(queue) = queue(slot) {
        queue.lock().clear();
    }
}

/// Entries still held and every slot's count (tests).
#[cfg(lazyos_tests)]
pub fn len(slot: usize) -> usize {
    queue(slot).map_or(0, |queue| queue.lock().len())
}
