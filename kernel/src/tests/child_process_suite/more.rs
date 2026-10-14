//! Child ownership across threads, continued (issue #859): the native child
//! bell and a blocking `wait4` on a sibling thread, a parent-death signal
//! through the timer sweep, a chain of parents that all ask for `SIGKILL`
//! (delivery must not recurse), and a process whose leader exits first.

use super::*;
use crate::task::wait::CHILD_EXIT;
use crate::task::{childbell, WaitKind};

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

fn send_as_kernel(target: usize, sig: u8) -> Result<(), String> {
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

/// `SIGCHLD` is pending for `slot`'s process.
fn sigchld_pending(slot: usize) -> bool {
    signal::pending(slot) & (1 << signal::SIGCHLD) != 0
}

/// The leader exits first: nobody is told (the parent could not reap it, and
/// its bell must not be satisfied by a zombie it cannot collect); when the
/// last thread exits, `SIGCHLD`, the child bell and `wait4` all fire.
pub fn last_thread_notifies_the_parent() -> Result<(), String> {
    fresh()?;
    let (parent, _) = process_with_threads(0)?;
    let child = fork_as(parent)?;
    here(child);
    let thread =
        task::spawn_thread("late-thread", process::USER_STACK_TOP, 0, 0).map_err(String::from)?;
    here(KERNEL_TASK);
    check!(
        !childbell::arm(parent),
        "a running child satisfied the bell"
    );

    crate::task::family::reset_notifications();
    task::harness::finish(child, 0);
    check!(
        crate::task::family::notifications(parent) == 0,
        "the parent was told at the leader's exit"
    );
    check!(
        !sigchld_pending(parent),
        "SIGCHLD before the process was reapable"
    );
    check!(
        childbell::armed(parent),
        "the bell rang for a leader that cannot be reaped"
    );
    check!(
        !childbell::arm(parent),
        "arm() is ready for a zombie leader with a live thread"
    );
    check!(wait4_as(parent).0 == 0, "wait4 collected a zombie leader");

    task::harness::finish(thread, 0);
    check!(
        sigchld_pending(parent),
        "no SIGCHLD when the last thread exited"
    );
    check!(
        !childbell::armed(parent),
        "the bell was not rung by the last thread"
    );
    check!(
        crate::task::family::notifications(parent) == 1,
        "the parent was told {} times",
        crate::task::family::notifications(parent)
    );
    check!(
        childbell::arm(parent),
        "arm() is not ready for the reapable child"
    );
    childbell::disarm(parent);
    let (ret, _) = wait4_as(parent);
    check!(ret == child as u64, "wait4 returned {ret:#x}");
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// `SIGKILL` of a process with a thread blocked in the kernel: the leader
/// ends at once and tells nobody; the thread leaves the kernel later and
/// that is when the parent hears of it (status: killed by SIGKILL).
pub fn sigkill_with_a_thread_in_the_kernel() -> Result<(), String> {
    use crate::task::wait::WaitQueue;
    fresh()?;
    let queue = WaitQueue::new(WaitKind::Block);
    let (parent, _) = process_with_threads(0)?;
    let child = fork_as(parent)?;
    here(child);
    let thread = task::spawn_thread("blocked-thread", process::USER_STACK_TOP, 0, 0)
        .map_err(String::from)?;
    here(KERNEL_TASK);
    let cs = task::harness::set_kernel_frame(thread).ok_or("no frame")?;
    queue.park(thread, None);
    check!(!childbell::arm(parent), "bell ready with a running child");

    send_as_kernel(child, signal::SIGKILL)?;
    check!(
        task::harness::state(child) == Some(TaskState::Done)
            && task::harness::state(thread) == Some(TaskState::Runnable),
        "unexpected states: leader {:?}, thread {:?}",
        task::harness::state(child),
        task::harness::state(thread)
    );
    check!(
        !sigchld_pending(parent),
        "SIGCHLD while a thread was still in the kernel"
    );
    check!(
        childbell::armed(parent) && wait4_as(parent).0 == 0,
        "the parent was told, or could reap, before the thread left"
    );

    queue.forget(thread);
    task::harness::set_frame_cs(thread, cs);
    check!(
        task::harness::resume_delivery(thread),
        "the thread was not ended"
    );
    check!(
        sigchld_pending(parent),
        "no SIGCHLD once the thread left the kernel"
    );
    check!(!childbell::armed(parent), "the bell was not rung");
    let (ret, status) = wait4_as(parent);
    check!(
        ret == child as u64 && status & 0x7f == signal::SIGKILL as u32,
        "wait4 returned {ret:#x} status {status:#x}"
    );
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// A leader that exited normally reports the group's status when a thread
/// later ends the group (a fault path with status 3, then `SIGKILL`).
pub fn group_exit_status_reaches_a_zombie_leader() -> Result<(), String> {
    fresh()?;
    let (parent, _) = process_with_threads(0)?;
    for (group_status, want) in [(3u64, 3u32 << 8), (128 + signal::SIGKILL as u64, 9)] {
        let child = fork_as(parent)?;
        here(child);
        let thread = task::spawn_thread("group-thread", process::USER_STACK_TOP, 0, 0)
            .map_err(String::from)?;
        here(KERNEL_TASK);
        let pml4 = task::harness::pml4(child).ok_or("pml4")?;
        task::harness::finish(child, 0);
        if group_status == 3 {
            signal::terminate_process(pml4, group_status);
        } else {
            send_as_kernel(thread, signal::SIGKILL)?;
        }
        check!(
            task::harness::state(thread) == Some(TaskState::Done),
            "the thread survived the group exit"
        );
        let (ret, status) = wait4_as(parent);
        check!(
            ret == child as u64 && status == want,
            "group status {group_status}: wait4 returned {ret:#x} status {status:#x}, want {want:#x}"
        );
    }
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// A non-leader thread that execs becomes its own leader (`getpid` follows)
/// and, as the last live task of the old process, makes a zombie leader
/// reapable: the parent is told.
pub fn exec_from_the_last_thread() -> Result<(), String> {
    fresh()?;
    let (parent, _) = process_with_threads(0)?;
    let child = fork_as(parent)?;
    here(child);
    let thread =
        task::spawn_thread("exec-thread", process::USER_STACK_TOP, 0, 0).map_err(String::from)?;
    here(KERNEL_TASK);
    task::harness::finish(child, 0);
    check!(!sigchld_pending(parent), "SIGCHLD before the exec");
    here(thread);
    check!(
        task::linuxstate::tgid() == child,
        "the thread's pid is not the leader's"
    );
    let table = crate::mem::new_user_table().ok_or("no table")?;
    task::set_pml4(table.as_u64());
    check!(
        task::linuxstate::tgid() == thread,
        "the exec'd thread is not its own leader"
    );
    here(KERNEL_TASK);
    check!(
        sigchld_pending(parent),
        "no SIGCHLD when the old process lost its last task"
    );
    let (ret, _) = wait4_as(parent);
    check!(
        ret == child as u64,
        "the zombie leader was not reapable: {ret:#x}"
    );
    task::harness::finish(thread, 0);
    task::harness::simulate_tick();
    here(KERNEL_TASK);
    task::reclaim_pending();
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// `kill_group` of a process group holding a zombie leader and its thread:
/// the parent is told once, when the last of them goes, not at the leader.
pub fn kill_group_with_a_zombie_leader() -> Result<(), String> {
    use crate::task::family::{notifications, reset_notifications};
    fresh()?;
    let (parent, _) = process_with_threads(0)?;
    let child = fork_as(parent)?;
    // Its own process group, so the parent is not in the one killed.
    task::process::setpgid(parent, child as i64, 0).map_err(|e| format!("setpgid: {e:?}"))?;
    here(child);
    let thread =
        task::spawn_thread("group-thread", process::USER_STACK_TOP, 0, 0).map_err(String::from)?;
    here(KERNEL_TASK);
    task::harness::finish(child, 0);
    reset_notifications();
    check!(
        task::process::pgid_of(thread) == task::process::pgid_of(child),
        "the thread left its group"
    );
    check!(
        task::kill_group(task::process::pgid_of(child)) == 1,
        "kill_group should have ended the one live thread"
    );
    check!(
        notifications(parent) == 1,
        "the parent was told {} times",
        notifications(parent)
    );
    check!(sigchld_pending(parent), "no SIGCHLD from kill_group");
    let (ret, _) = wait4_as(parent);
    check!(ret == child as u64, "wait4 returned {ret:#x}");
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// The ordinary `exit_group` path (`exit_thread_group`): the main thread left
/// first with status 0, a thread then ends the group with status 3, and the
/// parent hears once and reaps the group's status.
pub fn exit_group_status_reaches_a_zombie_leader() -> Result<(), String> {
    use crate::task::family::{notifications, reset_notifications};
    fresh()?;
    let (parent, _) = process_with_threads(0)?;
    let child = fork_as(parent)?;
    here(child);
    let thread =
        task::spawn_thread("exit-thread", process::USER_STACK_TOP, 0, 0).map_err(String::from)?;
    let other =
        task::spawn_thread("other-thread", process::USER_STACK_TOP, 0, 0).map_err(String::from)?;
    here(KERNEL_TASK);
    task::harness::finish(child, 0);
    reset_notifications();
    here(thread);
    task::exit_thread_group(3);
    here(KERNEL_TASK);
    check!(
        task::harness::state(other) == Some(TaskState::Done),
        "exit_group left a thread running"
    );
    check!(
        notifications(parent) == 1,
        "the parent was told {} times",
        notifications(parent)
    );
    let (ret, status) = wait4_as(parent);
    check!(
        ret == child as u64 && status == 3 << 8,
        "wait4 returned {ret:#x} status {status:#x}, want the group's 3"
    );
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}
