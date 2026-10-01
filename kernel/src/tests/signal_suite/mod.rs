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
mod hardening;
mod linux_abi;
mod native_kill;
mod suspend;
mod sweep_space;

pub(super) use delivery::*;
pub(super) use hardening::*;
pub(super) use linux_abi::*;
pub(super) use native_kill::*;
pub(super) use suspend::*;
pub(super) use sweep_space::*;

pub(super) const CASES: &[(&str, Test)] = &[
    ("task_signal_block_unblock", block_unblock_pending),
    ("task_signal_kill_wakes_sleeper", kill_wakes_blocked),
    ("task_signal_kill_uncatchable", sigkill_uncatchable),
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
];
