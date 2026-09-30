//! Task bookkeeping: registration, reaping, fd/futex plumbing, wait
//! queues, and the process tree (fork/reap, groups, sessions, signals
//! delivered through kill_group, and the introspection snapshot).

use super::*;
use crate::task::process::GroupError;

/// Fork from the kernel task until `spawn_fork` refuses, then finish and
/// reap every child. Returns how many children fit.
fn fill_and_drain() -> Result<usize, String> {
    let mut children = alloc::vec::Vec::new();
    loop {
        match task::spawn_fork() {
            Ok(slot) => children.push(slot),
            Err(_) => break,
        }
        check!(
            children.len() < task::MAX_TASKS,
            "spawn_fork handed out more slots than the table has"
        );
    }
    let spawned = children.len();
    for (index, slot) in children.iter().enumerate() {
        task::harness::finish(*slot, index as u64);
    }
    for _ in 0..spawned {
        task::reap_child().ok_or("a finished child is not reapable")?;
    }
    check!(
        task::reap_child().is_none(),
        "reap_child returned a child after the table was drained"
    );
    Ok(spawned)
}

/// Build `depth` nested `spawn_fork` children (`spawn_fork` forks the
/// current task, so the harness points `current()` at each new child) and
/// return their slots from root to leaf. Leaves `current()` at the leaf.
fn fork_chain(depth: usize) -> Result<Vec<usize>, String> {
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    let mut chain = Vec::new();
    for level in 0..depth {
        let slot = task::spawn_fork().map_err(|error| format!("level {level}: {error}"))?;
        chain.push(slot);
        task::harness::switch_current(slot);
    }
    Ok(chain)
}

/// Mark `slots` finished leaf-first (so each death re-parents its children)
/// and reap every one of them as init, then reset the table.
fn finish_and_reap_all(slots: &[usize]) -> Result<(), String> {
    for &slot in slots.iter().rev() {
        task::harness::finish(slot, 0);
    }
    task::harness::switch_current(task::KERNEL_TASK);
    let mut reaped = 0;
    while task::reap_child().is_some() {
        reaped += 1;
    }
    check!(
        reaped == slots.len(),
        "reaped {reaped} of {} finished tasks",
        slots.len()
    );
    task::harness::reset();
    Ok(())
}

mod direction_flag;
mod fpu_state;
mod lifecycle;
mod process_tree;
mod reclaim;
mod wait_queue;
mod yield_clock;

pub(super) use direction_flag::*;
pub(super) use fpu_state::*;
pub(super) use lifecycle::*;
pub(super) use process_tree::*;
pub(super) use reclaim::*;
pub(super) use wait_queue::*;
pub(super) use yield_clock::*;

pub(super) const CASES: &[(&str, Test)] = &[
    ("task_kernel_registered", kernel_registered),
    ("task_block_wake_roundtrip", block_wake_roundtrip),
    ("task_fork_reap_churn", fork_reap_churn),
    ("task_thread_exit_reclaim", thread_exit_reclaim),
    ("task_slots_fill_table", slots_fill_table),
    ("task_slots_soak_recycle", slots_soak_recycle),
    ("task_thread_churn_generations", thread_churn_generations),
    (
        "task_soak_thread_exit_generations",
        soak_thread_exit_generations,
    ),
    ("task_reclaim_wakes_pipe_peer", reclaim_wakes_pipe_peer),
    ("task_reclaim_pipe_close_soak", reclaim_pipe_close_soak),
    ("task_futex_wait_mismatch", futex_wait_mismatch),
    ("task_fd_table", fd_table),
    ("task_wait_queue_block_wake", wait_queue_block_wake),
    (
        "task_wait_queue_deadline_timeout",
        wait_queue_deadline_timeout,
    ),
    (
        "task_wait_queue_notify_all_order",
        wait_queue_notify_all_order,
    ),
    (
        "task_wait_queue_blocked_not_scheduled",
        wait_queue_blocked_not_scheduled,
    ),
    ("task_yield_does_not_tick", yield_does_not_tick),
    (
        "task_entry_clears_direction_flag",
        entry_clears_direction_flag,
    ),
    ("task_entry_direction_flag_soak", entry_direction_flag_soak),
    ("task_fpu_reset_is_default", fpu_reset_is_default),
    (
        "task_fpu_switch_keeps_each_tasks_state",
        fpu_switch_keeps_each_tasks_state,
    ),
    (
        "task_fpu_inherit_then_exec_reset",
        fpu_inherit_then_exec_reset,
    ),
    (
        "task_fpu_real_switch_to_a_user_task",
        fpu_real_switch_to_a_user_task,
    ),
    ("task_fpu_switch_soak_all_slots", fpu_switch_soak_all_slots),
    (
        "task_voluntary_park_expires_deadline",
        voluntary_park_expires_deadline,
    ),
    (
        "task_voluntary_entry_sweeps_other_deadlines",
        voluntary_entry_sweeps_other_deadlines,
    ),
    ("task_process_tree_fork", process_tree_fork),
    ("task_pgid_sid_inherit", pgid_sid_inherit),
    ("task_setsid_new_session", setsid_new_session),
    ("task_reparent_on_death", reparent_on_death),
    ("task_kill_group_terminates", kill_group_terminates),
    ("task_process_list_snapshot", process_list_snapshot),
    (
        "task_snapshot_matches_process_list",
        task_snapshot_matches_process_list,
    ),
    (
        "task_snapshot_soak_fork_churn",
        task_snapshot_soak_fork_churn,
    ),
];
