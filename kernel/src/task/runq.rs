//! Run queues (docs/performance-plan.md P6.1).
//!
//! The task table is the truth; these masks are an index over it so the
//! scheduler's per-entry work costs the number of runnable (or finished)
//! tasks, not [`MAX_TASKS`]:
//!
//! * `RUNNABLE[rank]`: the `Runnable` slots of each priority class.
//!   [`pick_next_best`](super::pick_next_best) and
//!   [`virtual_now`](super::virtual_now) walk only these;
//! * `DONE`: slots holding a finished task (a zombie or a parentless task
//!   awaiting reclamation), which the scheduler flags for `reclaim_pending`.
//!
//! Every write of a task's `state` or `class`, and every insertion into or
//! removal from the table, is followed by [`sync`] for that slot, under the
//! table lock, so the masks change exactly when the table does. A mask bit
//! the table no longer backs is harmless (readers re-check the slot and drop
//! it); a missing bit would hide a runnable task, so test builds compare the
//! masks with a full scan at every scheduler entry ([`verify`]) and panic on
//! the first difference.
//!
//! The masks are atomic only so they need no lock of their own: every write
//! happens with the task table held, and the scheduler reads them with it
//! held too.

use super::slotmask::{SlotMask, TakenSlots};
use super::{PriorityClass, Task, TaskState, MAX_TASKS};

/// One mask per scheduling class, indexed by `PriorityClass::rank`.
static RUNNABLE: [SlotMask; 4] = [const { SlotMask::new() }; 4];
/// Slots whose task is `Done`.
static DONE: SlotMask = SlotMask::new();

const _: () = assert!(PriorityClass::ALL.len() == 4);

/// Bring `slot`'s mask bits in line with the table. Call with the table held,
/// after any change to the slot's occupant, state or class.
pub(super) fn sync(tasks: &[Option<Task>; MAX_TASKS], slot: usize) {
    for mask in &RUNNABLE {
        mask.clear(slot);
    }
    DONE.clear(slot);
    let Some(task) = tasks.get(slot).and_then(|task| task.as_ref()) else {
        return;
    };
    match task.state {
        TaskState::Runnable => RUNNABLE[task.class.rank() as usize].set(slot),
        TaskState::Done => DONE.set(slot),
        TaskState::Blocked { .. } => {}
    }
}

/// [`sync`] every slot: after a bulk change (the test harness's reset).
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub(super) fn sync_all(tasks: &[Option<Task>; MAX_TASKS]) {
    for slot in 0..MAX_TASKS {
        sync(tasks, slot);
    }
}

/// The runnable slots of class `rank`, as recorded (re-check each one).
pub(super) fn runnable(rank: usize) -> TakenSlots {
    RUNNABLE[rank].snapshot()
}

/// The runnable slots of every class, ascending (re-check each one).
pub(super) fn runnable_any() -> TakenSlots {
    let mut all = RUNNABLE[0].snapshot();
    for mask in &RUNNABLE[1..] {
        all = all.union(&mask.snapshot());
    }
    all
}

/// The slots recorded as finished (re-check each one).
pub(super) fn done() -> TakenSlots {
    DONE.snapshot()
}

/// Whether `slot` is runnable in class `rank` according to the table; a stale
/// bit (none of today's paths leave one, see the module docs) is dropped.
pub(super) fn check(tasks: &[Option<Task>; MAX_TASKS], slot: usize, rank: usize) -> bool {
    let live = tasks[slot].as_ref().is_some_and(|task| {
        task.state == TaskState::Runnable && task.class.rank() as usize == rank
    });
    if !live {
        RUNNABLE[rank].clear(slot);
    }
    live
}

/// Test builds: the masks must equal a full scan of the table. A missing bit
/// is a runnable task the scheduler cannot see, so this panics with the slot.
#[cfg(lazyos_tests)]
pub(super) fn verify(tasks: &[Option<Task>; MAX_TASKS]) {
    for (slot, task) in tasks.iter().enumerate() {
        let runnable = task
            .as_ref()
            .filter(|task| task.state == TaskState::Runnable)
            .map(|task| task.class.rank() as usize);
        for (rank, mask) in RUNNABLE.iter().enumerate() {
            assert!(
                mask.contains(slot) == (runnable == Some(rank)),
                "runq: slot {slot} class {rank} mask disagrees with the table"
            );
        }
        let done = task
            .as_ref()
            .is_some_and(|task| task.state == TaskState::Done);
        assert!(
            DONE.contains(slot) == done,
            "runq: slot {slot} done mask disagrees with the table"
        );
    }
}
