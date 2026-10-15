//! Reading and changing a task's Linux scheduling attributes atomically
//! (`sched_setattr`, `sched_getattr`).
//!
//! A task's attributes live in two places: its stride weight in `Task::weight`
//! and its policy, nice value and `reset_on_fork` in `Task::linux.sched`. A
//! change reads the old state, decides (the rules compare new against old:
//! nothing may lower a nice value, leave `SCHED_IDLE` or clear
//! `reset_on_fork` without privilege) and then stores both. If any of that
//! took more than one hold of the task-table lock, two threads of one user
//! could each pass the checks against the state the other is about to replace
//! (one reads nice 5, another sets 10, the first then sets 7 and is allowed),
//! or the weight and the stored nice value could disagree. [`sched_update`]
//! therefore runs the decision under the same lock as both stores, and the
//! slot cannot be reaped and reused in between either.

use super::linuxstate::SchedState;
use super::*;
use crate::ipc::credentials::{self, Cred};

/// What [`sched_update`] shows its decision: the target as it is right now.
#[derive(Clone, Copy, Debug)]
pub struct SchedTarget {
    pub state: SchedState,
    /// The target's thread-group leader (a process's own slot is its pid).
    pub leader: usize,
    pub cred: Cred,
}

/// Why [`sched_update`] changed nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UpdateError<E> {
    /// The slot holds no task.
    NoSuchTask,
    /// The decision refused, with its own reason.
    Refused(E),
}

/// What `slot` has set (the default for a task that never called), or `None`
/// when the slot holds no task.
pub fn sched_read(slot: usize) -> Option<SchedState> {
    TASKS
        .lock()
        .get(slot)?
        .as_ref()
        .map(|task| task.linux.sched)
}

/// The stored attributes and the live weight of `slot` in one hold of the
/// task-table lock, so they are a consistent pair (tests and diagnostics).
#[cfg(lazyos_tests)]
pub fn sched_snapshot(slot: usize) -> Option<(SchedState, u16)> {
    TASKS
        .lock()
        .get(slot)?
        .as_ref()
        .map(|task| (task.linux.sched, task.weight))
}

/// Change `slot`'s scheduling attributes: `decide` sees the target as it is
/// and returns the state to store (or the reason to refuse); the state and the
/// weight it implies are then stored, all under one hold of the task-table
/// lock. `decide` runs with that lock held: it must not allocate, block or
/// call back into the task table.
///
/// The weight is the nice value's ([`nice`]) from the class the task is
/// assigned, `SCHED_IDLE` the smallest; a task the CPU guard has demoted gets
/// it when its window ends ([`guard::State::assign_weight`]).
pub fn sched_update<E>(
    slot: usize,
    decide: impl FnOnce(SchedTarget) -> Result<SchedState, E>,
) -> Result<(), UpdateError<E>> {
    let mut tasks = TASKS.lock();
    let leader = family::leader_of(&tasks, slot);
    let task = tasks
        .get_mut(slot)
        .and_then(|task| task.as_mut())
        .ok_or(UpdateError::NoSuchTask)?;
    let target = SchedTarget {
        state: task.linux.sched,
        leader,
        // The credential table has its own lock, taken after the task table's
        // everywhere (task creation stamps it under this one).
        cred: credentials::of(slot),
    };
    let new = decide(target).map_err(UpdateError::Refused)?;
    task.linux.sched = new;
    let base = task.guard.home(task.class).default_weight();
    let weight = nice::weight(base, i32::from(new.nice), new.is_idle());
    task.guard.assign_weight(&mut task.weight, weight);
    Ok(())
}
