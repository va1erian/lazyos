//! A thread woken for a process-directed signal that a sibling then takes has
//! nothing to deliver: its interrupted call must go on, not return `EINTR`
//! (Linux restarts it, `ERESTARTSYS`/`ERESTARTNOHAND` with no handler run).

use super::group::*;
use super::*;
use crate::task::signal::{self, UserRegs};
use crate::task::wait::WaitQueue;
use crate::task::{WaitKind, WakeReason};

const READ: u64 = 0;
const AFTER_SYSCALL: u64 = 0x0040_0502;
const EINTR_RET: u64 = (-4i64) as u64;

/// Thread A sleeps in a restartable call (a `read`); the leader blocks the
/// signal, so a process-directed `kill` aimed at A is taken by thread C in the
/// sweep, and A, woken for it, has no signal to take. It must not report
/// `EINTR`: its frame goes back to the `syscall` with `read`'s number, with
/// no handler involved. A signal A does take is unchanged.
pub fn threadsig_sibling_takes_the_signal_without_interrupting() -> Result<(), String> {
    let g = group(2)?;
    let (a, c) = (g.threads[0], g.threads[1]);
    check!(
        process::linux::restartable_for_test(READ),
        "read is not restartable"
    );
    signal::set_blocked(g.leader, kbit(USR1));
    let queue = WaitQueue::new(WaitKind::Sleep);
    queue.park(a, None);
    let user_cs = task::harness::set_kernel_frame(a).ok_or("no frame")?;
    check!(
        as_thread(task::KERNEL_TASK, KILL, &[a as u64, USR1 as u64]) == 0,
        "kill"
    );
    check!(
        task::harness::take_wake_reason(a) == Some(WakeReason::Interrupted),
        "the target of the signal was not woken"
    );
    g.sweep();
    check!(
        g.entered_members() == vec![c],
        "entered {:?}, expected C ({c}) to take the signal",
        g.entered_members()
    );
    // A has no handler to run and nothing pending: the call is re-issued.
    check!(!signal::wait_interrupted(a), "A still has a signal to take");
    let mut frame = UserRegs {
        rip: AFTER_SYSCALL,
        rax: EINTR_RET,
        ..UserRegs::default()
    };
    let plan =
        signal::restart_plan(EINTR_RET, Some(READ)).ok_or("EINTR of read is not replayed")?;
    signal::rewind_unhandled(&mut frame, plan);
    check!(
        frame.rip == AFTER_SYSCALL - 2 && frame.rax == READ,
        "the frame resumes at {:#x} with rax {:#x}",
        frame.rip,
        frame.rax
    );

    // A signal aimed at A itself is A's to take: it has something to deliver.
    queue.park(a, None);
    check!(tgkill(&g, task::KERNEL_TASK, a, USR2) == 0, "tgkill A");
    check!(
        task::harness::take_wake_reason(a) == Some(WakeReason::Interrupted)
            && signal::wait_interrupted(a),
        "A was not interrupted by its own signal"
    );
    task::harness::set_frame_cs(a, user_cs);
    queue.notify_all();
    teardown(&g)
}
