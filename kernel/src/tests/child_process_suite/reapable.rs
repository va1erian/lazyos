//! When a process becomes reapable (issue #859): the parent hears of a
//! process (`SIGCHLD`, the child bell, `wait4`) once, when its leader is gone
//! and its last thread has left; the group's exit status reaches a leader
//! that exited first; `exec` from the last thread; a group kill.

use super::more::send_as_kernel;
use super::*;
use crate::task::{childbell, WaitKind};

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

/// A group kill (`signal::terminate_process`) of a process holding a zombie
/// leader and its thread:
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
    let pml4 = task::harness::pml4(child).ok_or("no address space")?;
    check!(
        signal::terminate_process(pml4, 137) == 1,
        "the group kill should have ended the one live thread"
    );
    check!(
        notifications(parent) == 1,
        "the parent was told {} times",
        notifications(parent)
    );
    check!(sigchld_pending(parent), "no SIGCHLD from the group kill");
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
