//! CPU ticks charged to the uid that consumed them (`Resource::CpuTicks`,
//! issue #483), and the scheduler's view of who is over its cap.
//!
//! The scheduler books every timer period to the running task
//! (`task::schedule::charge_tick`); [`charge_ticks`] then books it to that
//! task's uid. The usage only grows: it is CPU time consumed, and the limit
//! is a budget. A uid past its budget is not stopped, it is deprioritised:
//! [`over_cap`] tells the stride scheduler to charge its tasks
//! [`OVER_CAP_STRIDE`] times the virtual time per quantum, so an uncapped
//! peer of the same class and weight gets that many more ticks.
//!
//! The tick runs in an interrupt and may land on a kernel thread holding the
//! credentials or the quota table, so both are only *tried*: a busy table
//! parks the ticks in [`PENDING`] for the next tick, and nothing spins in the
//! handler.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{entry_for, Resource, QUOTAS};
use crate::task::MAX_TASKS;

/// How many times the normal stride an over-cap task pays per quantum.
pub const OVER_CAP_STRIDE: u64 = 8;

/// Ticks a slot consumed that could not be booked yet (a table was busy).
static PENDING: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];
/// Whether the uid of the task in each slot was over its CPU cap at its last
/// booked tick.
static OVER: [AtomicBool; MAX_TASKS] = [const { AtomicBool::new(false) }; MAX_TASKS];

/// Book `ticks` consumed by the task in `slot` to its uid. Called from the
/// scheduler with interrupts off; never waits for a lock.
pub fn charge_ticks(slot: usize, ticks: u64) {
    let (Some(pending), Some(over)) = (PENDING.get(slot), OVER.get(slot)) else {
        return;
    };
    let ticks = ticks.saturating_add(pending.swap(0, Ordering::Relaxed));
    if ticks == 0 {
        return;
    }
    let Some(cred) = crate::ipc::credentials::try_of(slot) else {
        pending.fetch_add(ticks, Ordering::Relaxed);
        return;
    };
    let Some(mut quotas) = QUOTAS.try_lock() else {
        pending.fetch_add(ticks, Ordering::Relaxed);
        return;
    };
    let entry = entry_for(&mut quotas, cred.uid);
    let index = Resource::CpuTicks.index();
    entry.usage[index] = entry.usage[index].saturating_add(ticks);
    entry.peak[index] = entry.peak[index].max(entry.usage[index]);
    entry.charges += 1;
    over.store(entry.usage[index] > entry.limits[index], Ordering::Relaxed);
}

/// Whether the task in `slot` belongs to a uid past its CPU budget.
pub fn over_cap(slot: usize) -> bool {
    OVER.get(slot)
        .is_some_and(|over| over.load(Ordering::Relaxed))
}

/// Forget `slot`'s booking state: a new task (or a new identity) starts in
/// good standing until its first booked tick says otherwise.
pub fn forget_slot(slot: usize) {
    if let (Some(pending), Some(over)) = (PENDING.get(slot), OVER.get(slot)) {
        pending.store(0, Ordering::Relaxed);
        over.store(false, Ordering::Relaxed);
    }
}

/// Clear every slot's state (part of [`super::reset`]).
pub(super) fn reset() {
    for slot in 0..MAX_TASKS {
        forget_slot(slot);
    }
}
