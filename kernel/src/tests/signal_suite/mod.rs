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
mod linux_abi;

pub(super) use delivery::*;
pub(super) use linux_abi::*;

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
];
