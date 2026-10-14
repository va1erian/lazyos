//! A child belongs to its parent's process, not to the thread that forked it
//! (issue #859): any thread may `wait4` for it (WebKit forks on one thread and
//! reaps on another), it stays with the process when the forking thread
//! exits, `PR_SET_PDEATHSIG` fires when the parent process dies, and a socket
//! whose peer is gone is readable (EOF).

use super::*;
use crate::task::signal::{self, Disposition};
use crate::task::{TaskState, KERNEL_TASK};
use alloc::string::String;

const WAIT4: u64 = 61;
const PRCTL: u64 = 157;
const POLL_CALL: u64 = 7;
const WNOHANG: u64 = 1;
const ECHILD: u64 = (-10i64) as u64;
const EINVAL: u64 = (-22i64) as u64;
const PR_SET_PDEATHSIG: u64 = 1;
const PR_GET_PDEATHSIG: u64 = 2;
const POLLIN: u16 = 0x1;
const POLLHUP: u16 = 0x10;
const POLLRDHUP: u16 = 0x2000;

mod more;
mod reapable;

fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(KERNEL_TASK);
    signal::harness::reset();
    Ok(())
}

fn here(slot: usize) {
    task::harness::switch_current(slot);
}

/// A process (a child of the kernel task) and `threads` threads of it.
fn process_with_threads(threads: usize) -> Result<(usize, Vec<usize>), String> {
    let leader = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    here(leader);
    let mut made = Vec::new();
    for _ in 0..threads {
        let thread = task::spawn_thread("child-thread", process::USER_STACK_TOP, 0, 0)
            .map_err(|error| format!("thread: {error}"))?;
        made.push(thread);
    }
    here(KERNEL_TASK);
    Ok((leader, made))
}

/// `slot` forks a child of its process.
fn fork_as(slot: usize) -> Result<usize, String> {
    here(slot);
    let child = task::spawn_fork().map_err(|error| format!("fork: {error}"));
    here(KERNEL_TASK);
    child
}

/// `wait4(-1, &status, WNOHANG)` as `slot`: `(result, status)`.
fn wait4_as(slot: usize) -> (u64, u32) {
    here(slot);
    let mut status = 0u32;
    let ret = process::linux::dispatch_args_for_test(
        WAIT4,
        u64::MAX,
        &mut status as *mut u32 as u64,
        WNOHANG,
        0,
    );
    here(KERNEL_TASK);
    (ret, status)
}

/// Fork on one thread, `wait4` from another thread and from the leader; a
/// stranger gets `ECHILD`; `getppid` names the process, not the thread.
pub fn wait4_from_a_sibling_thread() -> Result<(), String> {
    fresh()?;
    let (browser, threads) = process_with_threads(2)?;
    let (forker, reaper) = (threads[0], threads[1]);
    let stranger = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let child = fork_as(forker)?;
    check!(
        task::process::ppid_of(child) == browser,
        "the child's parent is {}, not the process {browser}",
        task::process::ppid_of(child)
    );
    for slot in [browser, forker, reaper] {
        here(slot);
        check!(
            task::has_child_filtered(task::ChildFilter::Any) && task::is_child(child),
            "thread {slot} does not see the child"
        );
        here(KERNEL_TASK);
        let (ret, _) = wait4_as(slot);
        check!(
            ret == 0,
            "wait4 of a running child returned {ret:#x} on {slot}"
        );
    }
    here(stranger);
    check!(
        !task::has_child_filtered(task::ChildFilter::Any) && !task::is_child(child),
        "a stranger sees the child"
    );
    here(KERNEL_TASK);
    check!(
        wait4_as(stranger).0 == ECHILD,
        "a stranger's wait4 was not ECHILD"
    );

    task::harness::finish(child, 7);
    let (ret, status) = wait4_as(stranger);
    check!(ret == ECHILD, "a stranger reaped the child: {ret:#x}");
    let _ = status;
    let (ret, status) = wait4_as(reaper);
    check!(
        ret == child as u64 && status == 7 << 8,
        "the sibling thread's wait4 returned {ret:#x} status {status:#x}"
    );
    check!(wait4_as(reaper).0 == ECHILD, "the child was reaped twice");
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// The forking thread exits: the children stay with the process (a sibling
/// can reap them, they are not init's) until its last thread exits.
pub fn children_outlive_the_forking_thread() -> Result<(), String> {
    fresh()?;
    let (browser, threads) = process_with_threads(2)?;
    let (a, b) = (threads[0], threads[1]);
    let c1 = fork_as(a)?;
    let c2 = fork_as(b)?;
    task::harness::finish(a, 0);
    check!(
        task::process::ppid_of(c1) == browser,
        "c1 went to {} when its forking thread exited",
        task::process::ppid_of(c1)
    );
    task::harness::finish(c1, 3);
    let (ret, _) = wait4_as(b);
    check!(ret == c1 as u64, "the sibling could not reap c1: {ret:#x}");
    task::harness::finish(b, 0);
    check!(
        task::process::ppid_of(c2) == browser,
        "c2 left the process with only one thread gone: {}",
        task::process::ppid_of(c2)
    );
    // The last thread: init adopts what is left.
    task::harness::finish(browser, 0);
    check!(
        task::process::ppid_of(c2) == 0,
        "c2 was not adopted by init: {}",
        task::process::ppid_of(c2)
    );
    task::harness::finish(c2, 0);
    // (The harness lets the kernel task collect thread corpses too.)
    let mut reaped = Vec::new();
    while let Some((slot, _)) = task::reap_child() {
        reaped.push(slot);
    }
    check!(
        reaped.contains(&c2),
        "init could not reap the adopted child: {reaped:?}"
    );
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// Soak: 1000 fork / exit / reap rounds across threads, some where the
/// forking thread exits before its child; no zombie is left for anyone.
pub fn soak_cross_thread_fork_reap() -> Result<(), String> {
    fresh()?;
    let (browser, threads) = process_with_threads(2)?;
    let before = crate::mem::frame_stats().live();
    for round in 0..1000usize {
        let fail = |what: &str| format!("round {round}: {what}");
        let (forker, reaper) = if round % 2 == 0 {
            (threads[0], threads[1])
        } else {
            (threads[1], browser)
        };
        let child = if round % 5 == 0 {
            // A short-lived thread forks and exits first.
            here(browser);
            let temp = task::spawn_thread("temp-thread", process::USER_STACK_TOP, 0, 0)
                .map_err(String::from)?;
            here(KERNEL_TASK);
            let child = fork_as(temp)?;
            task::harness::finish(temp, 0);
            check!(
                task::process::ppid_of(child) == browser,
                "{}",
                fail("the child left the process with its forking thread")
            );
            child
        } else {
            fork_as(forker)?
        };
        task::harness::finish(child, (round % 200) as u64);
        let (ret, status) = wait4_as(reaper);
        check!(
            ret == child as u64 && status == ((round % 200) as u32) << 8,
            "{}",
            fail(&format!("wait4 returned {ret:#x} status {status:#x}"))
        );
        for slot in [browser, threads[0], threads[1]] {
            check!(
                wait4_as(slot).0 == ECHILD,
                "{}",
                fail("a zombie or child is left")
            );
        }
        // Done threads go back to the table (a scheduler tick does it).
        task::harness::simulate_tick();
        here(KERNEL_TASK);
        task::reclaim_pending();
    }
    check!(
        task::process::children_of(browser).is_empty()
            && task::process::children_of(threads[0]).is_empty()
            && task::process::children_of(threads[1]).is_empty(),
        "children remain"
    );
    task::harness::reset();
    signal::harness::reset();
    let after = crate::mem::frame_stats().live();
    check!(after <= before + 16, "frames leaked: {before} -> {after}");
    Ok(())
}

fn prctl_as(slot: usize, option: u64, arg2: u64) -> u64 {
    here(slot);
    let ret = process::linux::dispatch_args_for_test(PRCTL, option, arg2, 0, 0);
    here(KERNEL_TASK);
    ret
}

fn get_pdeathsig(slot: usize) -> i32 {
    let mut sig = -1i32;
    prctl_as(slot, PR_GET_PDEATHSIG, &mut sig as *mut i32 as u64);
    sig
}

/// `PR_SET_PDEATHSIG`: stored per process, validated, cleared by `fork`;
/// sent when the last thread of the parent exits and not before.
pub fn pdeathsig_on_parent_exit() -> Result<(), String> {
    fresh()?;
    let (parent, threads) = process_with_threads(1)?;
    let child = fork_as(parent)?;
    check!(get_pdeathsig(child) == 0, "the default is not 0");
    check!(
        prctl_as(child, PR_SET_PDEATHSIG, 65) == EINVAL,
        "signal 65 was accepted"
    );
    check!(
        prctl_as(child, PR_SET_PDEATHSIG, signal::SIGUSR1 as u64) == 0
            && get_pdeathsig(child) == signal::SIGUSR1 as i32,
        "set/get failed"
    );
    // A thread of the child sets it for the process; a fork does not inherit.
    let grandchild = fork_as(child)?;
    check!(get_pdeathsig(grandchild) == 0, "fork inherited the signal");
    // Hold the signal back so it stays visible as pending.
    signal::set_blocked(child, 1 << signal::SIGUSR1);

    task::harness::finish(threads[0], 0);
    check!(
        signal::pending(child) & (1 << signal::SIGUSR1) == 0,
        "a thread's exit signalled the child"
    );
    task::harness::finish(parent, 0);
    check!(
        signal::pending(child) & (1 << signal::SIGUSR1) != 0,
        "the parent's exit did not signal the child"
    );
    check!(
        signal::pending(grandchild) == 0,
        "the grandchild was signalled by its grandparent"
    );
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// The parent is killed (`SIGKILL` of a process with threads): the child that
/// asked for `SIGKILL` dies with it, one that did not lives on, and the
/// signal is sent once.
pub fn pdeathsig_when_the_parent_is_killed() -> Result<(), String> {
    fresh()?;
    let (parent, threads) = process_with_threads(2)?;
    let doomed = fork_as(threads[1])?;
    let spared = fork_as(parent)?;
    check!(
        prctl_as(doomed, PR_SET_PDEATHSIG, signal::SIGKILL as u64) == 0,
        "prctl"
    );
    signal::send_to_slot(
        KERNEL_TASK,
        parent,
        signal::SIGKILL,
        signal::SigInfo::kernel(),
    )
    .map_err(|error| format!("kill: {error:?}"))?;
    for slot in [parent, threads[0], threads[1]] {
        check!(
            task::harness::state(slot) == Some(TaskState::Done),
            "slot {slot} of the killed process survived"
        );
    }
    check!(
        task::harness::state(doomed) == Some(TaskState::Done),
        "the child with PDEATHSIG=SIGKILL survived its parent"
    );
    check!(
        task::harness::state(spared) == Some(TaskState::Runnable),
        "a child without PDEATHSIG was killed: {:?}",
        task::harness::state(spared)
    );
    check!(
        task::process::ppid_of(spared) == 0,
        "the survivor was not adopted by init"
    );
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// Soak: 1000 parents that die with a child asking for a signal; the signal
/// arrives every time and no registry or table entry is left.
pub fn soak_pdeathsig() -> Result<(), String> {
    fresh()?;
    for round in 0..1000usize {
        let (parent, threads) = process_with_threads(round % 3)?;
        let child = fork_as(parent)?;
        check!(
            prctl_as(child, PR_SET_PDEATHSIG, signal::SIGUSR1 as u64) == 0,
            "round {round}: prctl"
        );
        signal::set_action(child, signal::SIGUSR1, Disposition::Default)
            .map_err(|error| format!("round {round}: {error:?}"))?;
        signal::set_blocked(child, 1 << signal::SIGUSR1);
        for thread in threads {
            task::harness::finish(thread, 0);
        }
        check!(
            signal::pending(child) == 0,
            "round {round}: signalled before the last thread exited"
        );
        task::harness::finish(parent, 0);
        check!(
            signal::pending(child) & (1 << signal::SIGUSR1) != 0,
            "round {round}: the parent's exit did not signal the child"
        );
        task::harness::finish(child, 0);
        while task::reap_child().is_some() {}
        task::harness::simulate_tick();
        here(KERNEL_TASK);
        task::reclaim_pending();
    }
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

fn poll_revents(fd: u64, events: u16) -> u16 {
    let mut entry = [0u8; 8];
    entry[..4].copy_from_slice(&(fd as i32).to_le_bytes());
    entry[4..6].copy_from_slice(&events.to_le_bytes());
    process::linux::dispatch_args_for_test(POLL_CALL, entry.as_mut_ptr() as u64, 1, 0, 0);
    u16::from_le_bytes([entry[6], entry[7]])
}

/// A socket whose peer is gone is readable (`POLLIN`, `POLLRDHUP` when asked,
/// `POLLHUP`); a pipe whose writer is gone reports `POLLHUP` alone.
pub fn socket_peer_gone_is_readable() -> Result<(), String> {
    fresh()?;
    for kind in [1u64, 5] {
        let mut sv = [0i32; 2];
        check!(
            process::linux::dispatch_args_for_test(53, 1, kind, 0, sv.as_mut_ptr() as u64) == 0,
            "socketpair {kind}"
        );
        let (a, b) = (sv[0] as u64, sv[1] as u64);
        check!(
            poll_revents(a, POLLIN | POLLRDHUP) == 0,
            "an open idle socket (type {kind}) is ready"
        );
        check!(task::fd_close(b as usize), "close the peer");
        let revents = poll_revents(a, POLLIN | POLLRDHUP);
        check!(
            revents & (POLLIN | POLLRDHUP | POLLHUP) == POLLIN | POLLRDHUP | POLLHUP,
            "type {kind}: peer gone reports {revents:#x}"
        );
        let revents = poll_revents(a, 0);
        check!(
            revents & POLLHUP != 0 && revents & (POLLIN | POLLRDHUP) == 0,
            "type {kind}: unasked events were reported: {revents:#x}"
        );
        check!(task::fd_close(a as usize), "close");
    }
    let mut fds = [0i32; 2];
    check!(
        process::linux::dispatch_for_test(22, fds.as_mut_ptr() as u64, 0, 0) == 0,
        "pipe"
    );
    check!(task::fd_close(fds[1] as usize), "close the writer");
    let revents = poll_revents(fds[0] as u64, POLLIN);
    check!(
        revents & POLLHUP != 0 && revents & POLLIN == 0,
        "a pipe with no writer reports {revents:#x} (Linux: POLLHUP only)"
    );
    check!(task::fd_close(fds[0] as usize), "close");
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "child_bell_rings_sibling_thread",
        more::bell_rings_a_sibling_thread,
    ),
    (
        "child_blocking_wait4_on_sibling",
        more::blocking_wait4_on_a_sibling,
    ),
    (
        "child_pdeathsig_timer_sweep",
        more::pdeathsig_through_the_timer_sweep,
    ),
    (
        "child_pdeathsig_chain_no_recursion",
        more::pdeathsig_chain_does_not_recurse,
    ),
    ("child_leader_exits_first", more::leader_exits_first),
    (
        "child_last_thread_notifies_parent",
        reapable::last_thread_notifies_the_parent,
    ),
    (
        "child_sigkill_thread_in_kernel",
        reapable::sigkill_with_a_thread_in_the_kernel,
    ),
    (
        "child_group_exit_status_zombie_leader",
        reapable::group_exit_status_reaches_a_zombie_leader,
    ),
    (
        "child_exec_from_last_thread",
        reapable::exec_from_the_last_thread,
    ),
    (
        "child_kill_group_zombie_leader",
        reapable::kill_group_with_a_zombie_leader,
    ),
    (
        "child_exit_group_status_zombie_leader",
        reapable::exit_group_status_reaches_a_zombie_leader,
    ),
    (
        "child_wait4_from_sibling_thread",
        wait4_from_a_sibling_thread,
    ),
    (
        "child_outlives_forking_thread",
        children_outlive_the_forking_thread,
    ),
    (
        "child_soak_cross_thread_fork_reap",
        soak_cross_thread_fork_reap,
    ),
    ("child_pdeathsig_on_parent_exit", pdeathsig_on_parent_exit),
    (
        "child_pdeathsig_parent_killed",
        pdeathsig_when_the_parent_is_killed,
    ),
    ("child_soak_pdeathsig", soak_pdeathsig),
    (
        "child_socket_peer_gone_readable",
        socket_peer_gone_is_readable,
    ),
];
