//! A console `read` blocked while children exit: `SIGCHLD` under its default
//! disposition never interrupts it (however many children exit), a caught
//! one does (`EINTR`), and then the handler's `SA_RESTART` decides whether
//! the `read` is issued again or returns `EINTR`
//! ([`signal::deliver_linux_restartable`]'s pieces). The harness has no live
//! scheduler, so the parent is parked on the console's wait queue the way
//! `read_console` parks and its wake reason is what `read_console` returns on.

use super::*;
use crate::task::signal::{self, Disposition, UserRegs};
use crate::task::{wait, TaskState, WakeReason};

const EINTR: u64 = (-4i64) as u64;
const READ: u64 = 0;
/// A user address just past a `syscall` instruction.
const AFTER_SYSCALL: u64 = 0x0040_1002;

/// A parent (forked from the kernel) and its pending console read.
fn parent() -> Result<usize, String> {
    task::harness::switch_current(task::KERNEL_TASK);
    task::spawn_fork().map_err(|error| format!("fork: {error}"))
}

/// A child of `parent` that exits while `parent` waits.
fn child_exits(parent: usize) -> Result<(), String> {
    task::harness::switch_current(parent);
    let child = task::spawn_fork().map_err(|error| format!("fork child: {error}"))?;
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::finish(child, 0);
    Ok(())
}

/// End the parked read: drain the queue (a wait removes itself on return,
/// which the harness never reaches), reap the children and forget the reason.
fn end_read(parent: usize) {
    wait::TERMINAL.notify(task::MAX_TASKS);
    let _ = task::harness::take_wake_reason(parent);
    task::harness::switch_current(parent);
    while task::reap_child().is_some() {}
    task::harness::switch_current(task::KERNEL_TASK);
}

fn handler(flags: u64) -> Disposition {
    Disposition::Handler {
        handler: 0x0040_0100,
        flags,
        restorer: 0x0040_0200,
        mask: 0,
    }
}

/// What the interrupted `read` becomes after the first handler: `(rip, rax)`
/// of the frame it returns to.
fn after_handler(flags: u64) -> (u64, u64) {
    let mut frame = UserRegs {
        rip: AFTER_SYSCALL,
        rax: EINTR,
        ..UserRegs::default()
    };
    let mut plan = signal::restart_plan(EINTR, Some(READ));
    signal::rewind_for_restart(&mut frame, flags, &mut plan);
    // A second handler in the same delivery must not rewind again.
    signal::rewind_for_restart(&mut frame, flags, &mut plan);
    (frame.rip, frame.rax)
}

/// Default `SIGCHLD`: three children exit during the read; it stays blocked
/// (the event is still recorded as pending).
pub fn console_read_ignores_default_sigchld() -> Result<(), String> {
    fresh()?;
    let reader = parent()?;
    wait::TERMINAL.park(reader, None);
    for round in 0..3 {
        child_exits(reader)?;
        check!(
            task::harness::take_wake_reason(reader).is_none(),
            "exit {round}: a default SIGCHLD interrupted the read"
        );
        check!(
            matches!(
                task::harness::state(reader),
                Some(TaskState::Blocked { .. })
            ),
            "exit {round}: the reader is no longer blocked"
        );
    }
    check!(
        signal::pending(reader) & (1 << signal::SIGCHLD) != 0,
        "the child exits were not recorded"
    );
    end_read(reader);
    task::harness::reset();
    Ok(())
}

/// A caught `SIGCHLD` interrupts the read; with `SA_RESTART` the frame goes
/// back to the `syscall` with `read`'s number, without it the read returns
/// `EINTR`. Only an interrupted, restartable call is replayed.
pub fn console_read_sigchld_restart() -> Result<(), String> {
    fresh()?;
    check!(
        process::linux::restartable_for_test(READ),
        "read is not restartable"
    );
    check!(
        signal::restart_plan(5, Some(READ)).is_none(),
        "a completed read was planned for a restart"
    );
    for (flags, want) in [
        (signal::SA_RESTART, (AFTER_SYSCALL - 2, READ)),
        (0, (AFTER_SYSCALL, EINTR)),
    ] {
        let reader = parent()?;
        signal::set_action(reader, signal::SIGCHLD, handler(flags))
            .map_err(|error| format!("set_action: {error:?}"))?;
        wait::TERMINAL.park(reader, None);
        child_exits(reader)?;
        check!(
            task::harness::take_wake_reason(reader) == Some(WakeReason::Interrupted),
            "flags {flags:#x}: a caught SIGCHLD did not interrupt the read"
        );
        check!(
            after_handler(flags) == want,
            "flags {flags:#x}: the handler left {:?}, want {want:?}",
            after_handler(flags)
        );
        end_read(reader);
    }
    task::harness::reset();
    Ok(())
}

/// Soak: hundreds of reads, each with children exiting under a default,
/// a restarting and an interrupting disposition in turn: the outcome is the
/// same every round and no slot or waiter is left behind.
pub fn console_read_sigchld_soak() -> Result<(), String> {
    fresh()?;
    let reader = parent()?;
    for round in 0..300u64 {
        let flags = [None, Some(signal::SA_RESTART), Some(0)][(round % 3) as usize];
        let disposition = flags.map_or(Disposition::Default, handler);
        signal::set_action(reader, signal::SIGCHLD, disposition)
            .map_err(|error| format!("round {round}: set_action: {error:?}"))?;
        wait::TERMINAL.park(reader, None);
        for _ in 0..(round % 4 + 1) {
            child_exits(reader)?;
        }
        let reason = task::harness::take_wake_reason(reader);
        let want = flags.map(|_| WakeReason::Interrupted);
        check!(reason == want, "round {round}: woke with {reason:?}");
        if let Some(flags) = flags {
            let restarted = after_handler(flags).1 == READ;
            check!(
                restarted == (flags != 0),
                "round {round}: restart {restarted}"
            );
        }
        end_read(reader);
    }
    check!(
        task::process::children_of(reader).is_empty(),
        "children were left unreaped"
    );
    task::harness::reset();
    Ok(())
}
