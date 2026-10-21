//! Child ownership across threads, continued (issue #859): the native child
//! bell and a blocking `wait4` on a sibling thread, a parent-death signal
//! through the timer sweep, a chain of parents that all ask for `SIGKILL`
//! (delivery must not recurse), and a process whose leader exits first.

use super::*;
use crate::task::childbell;
use crate::task::wait::CHILD_EXIT;

fn blocked(slot: usize) -> bool {
    matches!(task::harness::state(slot), Some(TaskState::Blocked { .. }))
}

/// A thread parked on the native child bell is woken by a child its sibling
/// forked, and an unrelated process's bell stays armed.
pub fn bell_rings_a_sibling_thread() -> Result<(), String> {
    fresh()?;
    let (browser, threads) = process_with_threads(2)?;
    let (forker, listener) = (threads[0], threads[1]);
    let stranger = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    check!(
        !childbell::arm(listener),
        "a finished child was already waiting"
    );
    check!(!childbell::arm(stranger), "the stranger had a child");
    let child = fork_as(forker)?;
    check!(
        childbell::armed(listener) && childbell::armed(stranger),
        "the bells were not armed"
    );
    task::harness::finish(child, 0);
    check!(
        !childbell::armed(listener),
        "the sibling thread's bell was not rung by its process's child"
    );
    check!(
        childbell::armed(stranger),
        "another process's bell was rung"
    );
    // Level-triggered: the unreaped child satisfies the next arm, for any
    // thread of the process and for no other.
    check!(
        childbell::arm(browser),
        "the leader does not see the finished child"
    );
    check!(
        !childbell::arm(stranger),
        "the stranger sees the finished child"
    );
    childbell::disarm(stranger);
    let (ret, _) = wait4_as(listener);
    check!(ret == child as u64, "the listener could not reap: {ret:#x}");
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// A thread blocked in `wait4` (parked on the child-exit queue) is woken by
/// the exit of a child its sibling forked, and then reaps it.
pub fn blocking_wait4_on_a_sibling() -> Result<(), String> {
    fresh()?;
    let (_browser, threads) = process_with_threads(2)?;
    let (forker, waiter) = (threads[0], threads[1]);
    let child = fork_as(forker)?;
    CHILD_EXIT.park(waiter, None);
    check!(blocked(waiter), "the waiter is not parked");
    task::harness::finish(child, 4);
    check!(
        !blocked(waiter),
        "the child's exit did not wake the sibling thread's wait4"
    );
    let (ret, status) = wait4_as(waiter);
    check!(
        ret == child as u64 && status == 4 << 8,
        "wait4 returned {ret:#x} status {status:#x}"
    );
    CHILD_EXIT.forget(waiter);
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// A thread blocked in `waitid(P_PID, child, WEXITED | WNOWAIT)` (the launcher's
/// reaper, issue #887) is woken by the exit of a child its sibling forked, is
/// told which one and how it ended, and leaves it for another thread to
/// collect: the child is still there afterwards, and gone once collected.
pub fn blocking_waitid_nowait_on_a_sibling() -> Result<(), String> {
    const WAITID: u64 = 247;
    const P_PID: u64 = 1;
    const WEXITED: u64 = 4;
    const WNOWAIT: u64 = 0x0100_0000;
    let waitid_as = |slot: usize, child: usize, info: &mut [u8; 128]| {
        here(slot);
        let ret = process::linux::dispatch_args5_for_test(
            WAITID,
            P_PID,
            child as u64,
            info.as_mut_ptr() as u64,
            WEXITED | WNOWAIT,
            0,
        );
        here(KERNEL_TASK);
        ret
    };
    fresh()?;
    let (browser, threads) = process_with_threads(2)?;
    let (forker, waiter) = (threads[0], threads[1]);
    let child = fork_as(forker)?;
    CHILD_EXIT.park(waiter, None);
    check!(blocked(waiter), "the waiter is not parked");
    task::harness::finish(child, 5);
    check!(
        !blocked(waiter),
        "the child's exit did not wake the sibling thread's waitid"
    );
    let mut info = [0u8; 128];
    check!(
        waitid_as(waiter, child, &mut info) == 0,
        "waitid(WNOWAIT) of the sibling's child failed"
    );
    let word = |at: usize| i32::from_le_bytes(info[at..at + 4].try_into().unwrap_or([0; 4]));
    check!(
        word(16) == child as i32 && word(8) == 1 && word(24) == 5,
        "waitid said pid {} code {} status {}",
        word(16),
        word(8),
        word(24)
    );
    // Still a child of the process, for the leader (or the waiter again) to collect.
    check!(
        waitid_as(forker, child, &mut info) == 0,
        "the peek collected the child"
    );
    let (ret, status) = wait4_as(browser);
    check!(
        ret == child as u64 && status == 5 << 8,
        "the leader reaped {ret:#x} status {status:#x}"
    );
    check!(
        waitid_as(waiter, child, &mut info) == ECHILD,
        "a collected child is still waitable"
    );
    CHILD_EXIT.forget(waiter);
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// The parent dies on the timer sweep (a fatal signal that was pending,
/// unblocked): the sweep's finish path sends the parent-death signal too.
pub fn pdeathsig_through_the_timer_sweep() -> Result<(), String> {
    fresh()?;
    let (parent, _) = process_with_threads(0)?;
    let child = fork_as(parent)?;
    check!(
        prctl_as(child, PR_SET_PDEATHSIG, signal::SIGUSR1 as u64) == 0,
        "prctl"
    );
    signal::set_blocked(child, 1 << signal::SIGUSR1);
    // SIGTERM held back, then released: only the sweep can act on it.
    signal::set_blocked(parent, 1 << signal::SIGTERM);
    send_as_kernel(parent, signal::SIGTERM)?;
    check!(
        task::harness::state(parent) == Some(TaskState::Runnable),
        "a blocked SIGTERM ended the parent"
    );
    signal::set_blocked(parent, 0);
    task::harness::run_sweep();
    check!(
        task::harness::state(parent) == Some(TaskState::Done),
        "the sweep did not end the parent: {:?}",
        task::harness::state(parent)
    );
    check!(
        signal::pending(child) & (1 << signal::SIGUSR1) != 0,
        "the sweep's finish path did not send the parent-death signal"
    );
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

pub(super) fn send_as_kernel(target: usize, sig: u8) -> Result<(), String> {
    signal::send_to_slot(KERNEL_TASK, target, sig, signal::SigInfo::kernel())
        .map_err(|error| format!("send {sig} to {target}: {error:?}"))
}

/// A chain of 200 processes, each asking for `SIGKILL` when its parent dies:
/// killing the first ends them all, and the delivery loops (the signal that
/// kills a process finishes it, which flags its children) instead of nesting
/// 200 deep on a 48 KiB kernel stack.
pub fn pdeathsig_chain_does_not_recurse() -> Result<(), String> {
    const LENGTH: usize = 200;
    fresh()?;
    let mut chain = Vec::new();
    let first = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    chain.push(first);
    for link in 1..LENGTH {
        let child = fork_as(chain[link - 1])?;
        check!(
            prctl_as(child, PR_SET_PDEATHSIG, signal::SIGKILL as u64) == 0,
            "prctl at link {link}"
        );
        chain.push(child);
    }
    send_as_kernel(first, signal::SIGKILL)?;
    for (link, &slot) in chain.iter().enumerate() {
        check!(
            task::harness::state(slot) == Some(TaskState::Done),
            "link {link} survived the death of its parent"
        );
    }
    let mut reaped = 0;
    while task::reap_child().is_some() {
        reaped += 1;
    }
    // Every link but the first was re-parented to init when its parent died.
    check!(reaped >= LENGTH - 1, "only {reaped} corpses reaped");
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// The leader exits before its threads: it stays a zombie (its slot is the
/// process's identity), the threads keep `getppid`, `PR_SET_PDEATHSIG` and
/// their children, another process is not touched by them, the parent cannot
/// reap the leader until the last thread is gone, and a parent-death signal
/// still reaches a child process whose leader is already a zombie.
pub fn leader_exits_first() -> Result<(), String> {
    fresh()?;
    let (parent, _) = process_with_threads(0)?;
    let (leader, threads) = process_with_threads(1)?;
    let (_, others) = process_with_threads(1)?;
    let thread = threads[0];
    let bystander = others[0];
    // `leader` is a child of the kernel task; make it the parent's child too.
    let child_process = fork_as(parent)?;
    let kid = fork_as(thread)?;

    task::harness::finish(leader, 0);
    check!(
        task::reap_child().map(|(slot, _)| slot) != Some(leader),
        "the zombie leader was reaped under its running thread"
    );
    check!(
        task::process::ppid_of(thread) == 0 && task::process::ppid_of(kid) == leader,
        "ppid after the leader exited: thread {}, kid {}",
        task::process::ppid_of(thread),
        task::process::ppid_of(kid)
    );
    check!(
        prctl_as(thread, PR_SET_PDEATHSIG, signal::SIGUSR1 as u64) == 0
            && get_pdeathsig(thread) == signal::SIGUSR1 as i32,
        "the surviving thread could not set PDEATHSIG"
    );
    check!(
        get_pdeathsig(bystander) == 0 && get_pdeathsig(parent) == 0,
        "the setting leaked into another process"
    );
    let (ret, _) = wait4_as(thread);
    check!(
        ret == 0,
        "the thread's wait4 for its own running child returned {ret:#x}"
    );

    // A child process whose leader exits first still gets the signal.
    check!(
        prctl_as(child_process, PR_SET_PDEATHSIG, signal::SIGUSR1 as u64) == 0,
        "prctl"
    );
    here(child_process);
    let sibling =
        task::spawn_thread("late-thread", process::USER_STACK_TOP, 0, 0).map_err(String::from)?;
    here(KERNEL_TASK);
    check!(
        get_pdeathsig(sibling) == signal::SIGUSR1 as i32,
        "a new thread did not carry the signal"
    );
    task::harness::finish(child_process, 0);
    signal::set_blocked(sibling, 1 << signal::SIGUSR1);
    task::harness::finish(parent, 0);
    check!(
        signal::pending(sibling) & (1 << signal::SIGUSR1) != 0,
        "the live thread of a zombie-led child was not signalled"
    );

    // The last thread goes: now the zombie leader can be reaped.
    task::harness::finish(thread, 0);
    let mut reaped = Vec::new();
    while let Some((slot, _)) = task::reap_child() {
        reaped.push(slot);
    }
    check!(
        reaped.contains(&leader),
        "the leader was not reapable once its threads were gone: {reaped:?}"
    );
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}
