//! Signals (issue #60).

use super::*;
use crate::task::signal::{self, Disposition, SigInfo};
use crate::task::{TaskState, WaitKind, WakeReason};

/// Each test starts from one runnable kernel task, an empty task table and
/// an empty signal registry.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    signal::harness::reset();
    check!(
        task::harness::state(task::current()) == Some(TaskState::Runnable),
        "kernel task is not runnable after reset: {:?}",
        task::harness::state(task::current())
    );
    Ok(())
}

fn send(target: usize, sig: u8) -> Result<(), String> {
    let me = task::current();
    signal::send_to_slot(me, target, sig, SigInfo::user(me, signal::SI_USER))
        .map_err(|error| format!("send {sig} to {target}: {error:?}"))
}

mod delivery;
mod fpstate;
mod group_end;
mod hardening;
mod kill_parked;
mod killwatch;
mod linux_abi;
mod native_kill;
mod suspend;
mod sweep_space;
mod wait_interrupt;
mod xstate;

pub(super) use delivery::*;
pub(super) use fpstate::*;
pub(super) use group_end::*;
pub(super) use hardening::*;
pub(super) use kill_parked::*;
pub(super) use killwatch::*;
pub(super) use linux_abi::*;
pub(super) use native_kill::*;
pub(super) use suspend::*;
pub(super) use sweep_space::*;
pub(super) use wait_interrupt::*;
pub(super) use xstate::*;

pub(super) const CASES: &[(&str, Test)] = &[
    ("task_signal_fpstate_roundtrip", fpstate_frame_roundtrip),
    ("task_signal_fpstate_null_resets", fpstate_null_resets),
    ("task_signal_fpstate_forged", fpstate_forged_images),
    (
        "task_signal_fpstate_other_slot",
        fpstate_other_slot_uses_saved_area,
    ),
    ("task_signal_fpstate_soak", fpstate_soak),
    ("task_signal_xstate_frame_layout", xstate_frame_layout),
    (
        "task_signal_xstate_roundtrip_edit",
        xstate_roundtrip_and_edit,
    ),
    ("task_signal_xstate_forged_areas", xstate_forged_areas),
    ("task_signal_xstate_fxsave_frame", xstate_fxsave_frame),
    ("task_signal_xstate_soak", xstate_soak),
    (
        "task_signal_fpstate_sweep_follows_task",
        sweep_fpstate_follows_task,
    ),
    ("task_signal_block_unblock", block_unblock_pending),
    ("task_signal_kill_wakes_sleeper", kill_wakes_blocked),
    ("task_signal_kill_uncatchable", sigkill_uncatchable),
    (
        "task_signal_kill_defers_tasks_parked_in_kernel",
        kill_defers_tasks_parked_in_kernel,
    ),
    ("task_signal_soak_kill_parked", soak_kill_parked),
    (
        "task_signal_exit_group_spares_threads_in_kernel",
        exit_group_spares_threads_inside_the_kernel,
    ),
    (
        "task_signal_fault_spares_threads_in_kernel",
        fault_spares_threads_inside_the_kernel,
    ),
    (
        "task_signal_group_end_finishes_user_threads",
        group_end_finishes_user_threads_at_once,
    ),
    ("task_signal_group_end_soak", group_end_soak),
    (
        "task_signal_stop_reaches_the_native_boundary",
        stop_reaches_the_native_boundary,
    ),
    ("task_signal_stray_wakes_and_init", stray_wakes_and_init),
    (
        "task_signal_stop_spares_threads_in_kernel",
        stop_leaves_threads_inside_the_kernel_alone,
    ),
    (
        "task_signal_fatal_pending_follows_kill",
        fatal_pending_follows_the_kill,
    ),
    (
        "task_signal_kill_slow_names_stuck_thread",
        kill_slow_line_names_the_stuck_thread,
    ),
    (
        "task_signal_kill_slow_zombie_budget_exec",
        kill_slow_zombie_budget_and_exec,
    ),
    ("task_signal_soak_kill_every_state", soak_kill_every_state),
    ("task_signal_sigchld_child_exit", sigchld_on_child_exit),
    (
        "task_signal_handler_frame_roundtrip",
        handler_frame_roundtrip,
    ),
    ("task_signal_linux_sigset_roundtrip", linux_sigset_roundtrip),
    (
        "task_signal_linux_sigprocmask_boundary",
        linux_sigprocmask_sigset_boundary,
    ),
    ("task_signal_linux_sigset_soak", linux_sigset_translate_soak),
    ("task_signal_stop_continue", stop_continue),
    (
        "task_signal_sigreturn_frame_is_sanitised",
        sigreturn_frame_is_sanitised,
    ),
    (
        "task_signal_frame_arithmetic_is_checked",
        frame_arithmetic_is_checked,
    ),
    ("task_signal_soak_frame_validation", soak_frame_validation),
    (
        "task_signal_kill_needs_uid_or_capability",
        kill_needs_matching_uid_or_capability,
    ),
    ("task_signal_sigcont_within_session", sigcont_within_session),
    (
        "task_signal_kill_all_spares_init",
        kill_all_spares_init_and_respects_permissions,
    ),
    ("task_signal_soak_kill_permissions", soak_kill_permissions),
    ("task_signal_native_kill_syscall", native_kill_syscall_rules),
    ("task_signal_soak_native_kill", soak_native_kill),
    (
        "task_signal_native_sigterm_fatal_at_syscall_return",
        native_sigterm_fatal_at_syscall_return,
    ),
    ("task_signal_soak_native_sigterm", soak_native_sigterm),
    (
        "task_signal_suspend_swaps_and_restores_the_mask",
        suspend_swaps_and_restores_the_mask,
    ),
    (
        "task_signal_suspend_ignored_signals_and_owner",
        suspend_ignores_ignored_signals_and_is_per_task,
    ),
    ("task_signal_soak_suspend_cycles", soak_suspend_cycles),
    (
        "task_signal_sweep_frame_in_target_space",
        sweep_frame_in_target_space,
    ),
    (
        "task_signal_soak_sweep_space_isolation",
        soak_sweep_space_isolation,
    ),
    (
        "task_signal_wait_interrupted_by_linux_signals",
        wait_interrupted_by_linux_signals,
    ),
    ("task_signal_soak_wait_interrupted", soak_wait_interrupted),
];
