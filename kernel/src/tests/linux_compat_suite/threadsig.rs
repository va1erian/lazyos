//! Per-thread signal masks and thread-directed signals (issue #772): the
//! blocked mask and the alternate stack belong to a thread, `tkill`/`tgkill`
//! and faults queue on one thread, `kill` on the process (taken by one thread
//! that does not block it), and the dispositions stay per process.

use super::*;
use crate::task::signal::{self, Disposition, SigInfo};
use crate::task::wait::WaitQueue;
use crate::task::{TaskState, WaitKind, WakeReason};

mod group;
mod soak;

use group::*;
pub use soak::threadsig_soak;

/// A mask set on one thread changes no other thread's; a thread starts with
/// its creator's mask and a fork with the calling thread's; `ppoll`'s
/// temporary mask and `sigaltstack` are the caller's alone; `execve` keeps
/// the mask and drops the alternate stack.
pub fn threadsig_masks_belong_to_threads() -> Result<(), String> {
    let g = group(2)?;
    let (a, b) = (g.threads[0], g.threads[1]);
    let set = lbit(USR1);
    let block = |slot: usize, how: u64, set: &u64| {
        as_thread(slot, SIGPROCMASK, &[how, set as *const u64 as u64, 0, 8])
    };
    check!(
        block(a, signal::SIG_BLOCK, &set) == 0,
        "rt_sigprocmask(BLOCK)"
    );
    check!(
        blocked_of(a) == lbit(USR1),
        "A's mask is {:#x}",
        blocked_of(a)
    );
    check!(
        blocked_of(b) == 0 && blocked_of(g.leader) == 0,
        "A's mask leaked to B or the leader: {:#x} {:#x}",
        blocked_of(b),
        blocked_of(g.leader)
    );
    // The other `how`s go through the same per-thread state.
    let both = lbit(USR2) | lbit(USR1);
    check!(block(b, signal::SIG_SETMASK, &both) == 0, "SETMASK");
    check!(block(b, signal::SIG_UNBLOCK, &set) == 0, "UNBLOCK");
    check!(
        blocked_of(b) == lbit(USR2) && blocked_of(a) == lbit(USR1),
        "B {:#x}, A {:#x}",
        blocked_of(b),
        blocked_of(a)
    );

    // `clone` copies the creator's mask, `fork` the calling thread's.
    task::harness::switch_current(a);
    let from_a = thread()?;
    let child = task::spawn_fork().map_err(to_string)?;
    task::harness::switch_current(g.leader);
    let from_leader = thread()?;
    check!(
        signal::blocked(from_a) == kbit(USR1) && signal::blocked(child) == kbit(USR1),
        "a thread of A starts with {:#x}, A's fork with {:#x}",
        signal::blocked(from_a),
        signal::blocked(child)
    );
    check!(
        signal::blocked(from_leader) == 0,
        "a thread of the leader inherited A's mask"
    );

    // `ppoll`/`pselect6`/`epoll_pwait` swap the caller's mask for the wait.
    signal::suspend_begin(b, 0);
    check!(
        signal::blocked(b) == 0 && signal::blocked(a) == kbit(USR1),
        "a temporary mask reached another thread"
    );
    signal::suspend_end(b);
    check!(
        signal::blocked(b) == kbit(USR2),
        "the replaced mask did not come back"
    );

    // `sigaltstack` is per thread; a new thread has none, a fork keeps the caller's.
    let stack = [STACKS, 0, SPAN];
    check!(
        as_thread(b, SIGALTSTACK, &[stack.as_ptr() as u64, 0]) == 0,
        "sigaltstack"
    );
    check!(
        signal::altstack(b).enabled && !signal::altstack(a).enabled,
        "the alternate stack is not per thread"
    );
    task::harness::switch_current(b);
    let from_b = thread()?;
    let b_child = task::spawn_fork().map_err(to_string)?;
    check!(
        !signal::altstack(from_b).enabled,
        "a new thread inherited the alternate stack"
    );
    check!(
        signal::altstack(b_child) == signal::altstack(b),
        "fork lost the alternate stack"
    );

    // `execve` keeps the mask and the thread's pending signals, not the alternate stack.
    check!(tgkill(&g, g.leader, b, USR2) == 0, "tgkill");
    signal::exec_thread(b);
    check!(
        signal::blocked(b) == kbit(USR2)
            && signal::pending_thread(b) == kbit(USR2)
            && !signal::altstack(b).enabled,
        "exec: mask {:#x}, pending {:#x}, alternate stack {}",
        signal::blocked(b),
        signal::pending_thread(b),
        signal::altstack(b).enabled
    );

    // The forks are reaped by their parents.
    for (parent, child) in [(a, child), (b, b_child)] {
        task::harness::finish(child, 0);
        task::harness::switch_current(parent);
        task::reap_child().ok_or("a fork was not reapable")?;
    }
    reclaim_threads(&[from_a, from_leader, from_b]);
    teardown(&g)
}

/// `tgkill` runs the handler on the targeted thread alone, whatever its
/// siblings block; a target that blocks the signal keeps it until it
/// unblocks, and no other thread takes it meanwhile.
pub fn threadsig_tgkill_targets_one_thread() -> Result<(), String> {
    let g = group(3)?;
    let (a, b, c) = (g.threads[0], g.threads[1], g.threads[2]);
    signal::set_blocked(a, kbit(USR1));
    signal::set_blocked(c, kbit(USR1));
    check!(tgkill(&g, g.leader, b, USR1) == 0, "tgkill B");
    check!(
        signal::pending_thread(b) == kbit(USR1) && g.thread_pending() == kbit(USR1),
        "the signal is not queued on B alone: {:#x}",
        g.thread_pending()
    );
    check!(
        signal::pending(g.leader) & kbit(USR1) == 0,
        "a thread-directed signal reached the process"
    );
    g.sweep();
    check!(
        g.entered_members() == vec![b],
        "entered {:?}, expected only B ({b})",
        g.entered_members()
    );
    let regs = task::harness::frame_regs(b).ok_or("no frame")?;
    check!(regs.rdi == USR1 as u64, "handler argument {}", regs.rdi);
    check!(
        signal::blocked(b) & kbit(USR1) != 0,
        "the signal is not blocked while its handler runs"
    );
    g.return_from_handler(b, 0);
    check!(
        g.thread_pending() == 0 && signal::pending(b) == 0,
        "pending left after delivery"
    );

    // A target that blocks the signal keeps it; B, C and the leader never see it.
    check!(tgkill(&g, g.leader, a, USR1) == 0, "tgkill A");
    g.sweep();
    check!(
        g.entered_members().is_empty(),
        "someone took A's signal: {:?}",
        g.entered_members()
    );
    check!(
        signal::pending_thread(a) == kbit(USR1),
        "A lost the signal while blocking it"
    );
    signal::set_blocked(a, 0);
    g.sweep();
    check!(
        g.entered_members() == vec![a],
        "after unblocking: {:?}",
        g.entered_members()
    );
    g.return_from_handler(a, 0);

    // Argument errors, and the thread id the syscalls report.
    let wrong_group = [b as u64, c as u64, USR1 as u64];
    check!(
        as_thread(g.leader, TGKILL, &wrong_group) == ESRCH,
        "wrong tgid"
    );
    check!(
        as_thread(g.leader, TKILL, &[c as u64, 65]) == EINVAL,
        "signal 65"
    );
    check!(
        as_thread(g.leader, TKILL, &[250, USR1 as u64]) == ESRCH,
        "unused tid"
    );
    check!(
        as_thread(g.leader, TKILL, &[c as u64, 0]) == 0 && g.thread_pending() == 0,
        "signal 0 queued something"
    );
    check!(as_thread(c, GETTID, &[]) == c as u64, "gettid");
    teardown(&g)
}

/// A process-directed signal is taken by one thread that does not block it,
/// stays pending while all block it, and wakes the target thread when that one
/// may take it, else another that may.
pub fn threadsig_kill_lands_on_unblocked_thread() -> Result<(), String> {
    let g = group(2)?;
    let (a, b) = (g.threads[0], g.threads[1]);
    let pid = g.leader as u64;
    signal::set_blocked(g.leader, kbit(USR1));
    signal::set_blocked(a, kbit(USR1));
    check!(as_thread(a, KILL, &[pid, USR1 as u64]) == 0, "kill");
    check!(
        signal::pending(g.leader) == kbit(USR1) && g.thread_pending() == 0,
        "a process-directed signal queued on a thread"
    );
    g.sweep();
    check!(
        g.entered_members() == vec![b],
        "entered {:?}, expected only B ({b})",
        g.entered_members()
    );
    check!(
        signal::pending(g.leader) == 0,
        "still pending after B took it"
    );
    g.return_from_handler(b, 0);

    // Everyone blocks it: nothing is lost, the next thread to unblock takes it.
    signal::set_blocked(b, kbit(USR1));
    check!(
        as_thread(a, KILL, &[pid, USR1 as u64]) == 0,
        "kill (all blocked)"
    );
    g.sweep();
    check!(
        g.entered_members().is_empty() && signal::pending(a) == kbit(USR1),
        "a blocked process signal was delivered or lost: {:?} {:#x}",
        g.entered_members(),
        signal::pending(a)
    );
    signal::set_blocked(a, 0);
    g.sweep();
    check!(
        g.entered_members() == vec![a],
        "after A unblocked: {:?}",
        g.entered_members()
    );
    g.return_from_handler(a, 0);

    // Wake preference: the target thread when it may take the signal.
    let queues = [
        WaitQueue::new(WaitKind::Sleep),
        WaitQueue::new(WaitKind::Sleep),
        WaitQueue::new(WaitKind::Sleep),
    ];
    let park_all = || {
        for (queue, slot) in queues.iter().zip(g.members()) {
            queue.park(slot, None);
        }
    };
    signal::set_blocked(b, 0);
    park_all();
    check!(
        as_thread(task::KERNEL_TASK, KILL, &[pid, USR2 as u64]) == 0,
        "kill (parked)"
    );
    check!(
        task::harness::take_wake_reason(g.leader) == Some(WakeReason::Interrupted),
        "the target thread was not woken"
    );
    check!(
        is_blocked(a) && is_blocked(b),
        "threads besides the target woke"
    );
    // The target blocks it: the first thread that may take it is woken instead.
    for queue in &queues {
        queue.notify_all();
    }
    task::harness::take_wake_reason(a);
    task::harness::take_wake_reason(b);
    park_all();
    signal::set_blocked(g.leader, kbit(USR2));
    check!(
        as_thread(task::KERNEL_TASK, KILL, &[pid, USR2 as u64]) == 0,
        "kill (target blocks)"
    );
    check!(
        is_blocked(g.leader) && !is_blocked(a) && is_blocked(b),
        "wrong thread woken: leader blocked {}, A blocked {}, B blocked {}",
        is_blocked(g.leader),
        is_blocked(a),
        is_blocked(b)
    );
    for queue in &queues {
        queue.notify_all();
    }
    teardown(&g)
}

/// A thread parked in a wait that a thread-directed signal targets wakes with
/// `Interrupted`; its siblings stay parked, and so does a target that blocks it.
pub fn threadsig_wakes_only_the_target() -> Result<(), String> {
    let g = group(2)?;
    let (a, b) = (g.threads[0], g.threads[1]);
    let queues = [
        WaitQueue::new(WaitKind::Sleep),
        WaitQueue::new(WaitKind::Sleep),
        WaitQueue::new(WaitKind::Sleep),
    ];
    for (queue, slot) in queues.iter().zip(g.members()) {
        queue.park(slot, None);
    }
    check!(tgkill(&g, g.leader, b, USR1) == 0, "tgkill");
    check!(
        signal::wait_interrupted(b) && !signal::wait_interrupted(a),
        "only B has a signal to take"
    );
    check!(
        task::harness::take_wake_reason(b) == Some(WakeReason::Interrupted),
        "B was not interrupted"
    );
    check!(
        is_blocked(a) && is_blocked(g.leader),
        "a sibling woke for B's signal"
    );

    // A blocked target is left asleep, with the signal waiting on it.
    signal::set_blocked(a, kbit(USR2));
    check!(tgkill(&g, g.leader, a, USR2) == 0, "tgkill (blocked)");
    check!(is_blocked(a), "a blocked target was woken");
    check!(
        signal::pending_thread(a) == kbit(USR2) && !signal::wait_interrupted(a),
        "the blocked signal was not kept quietly"
    );
    for queue in &queues {
        queue.notify_all();
    }
    teardown(&g)
}

/// `rt_sigpending` reports the signals raised while blocked for the calling
/// thread: its own set and the process's.
pub fn threadsig_sigpending_is_the_union() -> Result<(), String> {
    let g = group(2)?;
    let (a, b) = (g.threads[0], g.threads[1]);
    signal::set_blocked(a, kbit(USR1) | kbit(USR2));
    check!(tgkill(&g, g.leader, a, USR1) == 0, "tgkill");
    check!(
        as_thread(task::KERNEL_TASK, KILL, &[g.leader as u64, USR2 as u64]) == 0,
        "kill"
    );
    let pending_of = |slot: usize| {
        let mut set = 0u64;
        let ret = as_thread(slot, SIGPENDING, &[&mut set as *mut u64 as u64, 8]);
        (ret, set)
    };
    check!(
        pending_of(a) == (0, lbit(USR1) | lbit(USR2)),
        "A reports {:#x?}",
        pending_of(a)
    );
    // B blocks nothing, so nothing is held back for it (and it would take the
    // process's signal itself on its next syscall: not asked through the call).
    check!(
        signal::pending_blocked(b) == 0,
        "B, which blocks nothing, holds back {:#x}",
        signal::pending_blocked(b)
    );
    task::harness::switch_current(a);
    check!(sys_checked(SIGPENDING, &[0, 8]) == EFAULT, "a null set");
    check!(
        sys(SIGPENDING, &[&mut 0u64 as *mut u64 as u64, 4]) == EINVAL,
        "a short set"
    );
    teardown(&g)
}

/// A synchronous fault is the faulting thread's: its own mask decides whether
/// the handler runs, whatever its siblings block.
pub fn threadsig_fault_uses_the_faulting_threads_mask() -> Result<(), String> {
    let g = group(2)?;
    let (a, b) = (g.threads[0], g.threads[1]);
    let handler = Disposition::Handler {
        handler: 0x0040_1000,
        flags: 0,
        restorer: 0x0040_2000,
        mask: 0,
    };
    signal::set_action(g.leader, signal::SIGSEGV, handler).map_err(|e| format!("{e:?}"))?;
    signal::set_blocked(a, kbit(signal::SIGSEGV));
    check!(
        !g.fault(a, 0xdead_0000)?,
        "a fault on a thread that blocks SIGSEGV was delivered"
    );
    check!(!g.entered(a), "A was entered");
    check!(
        g.fault(b, 0xdead_0000)?,
        "a fault on B, which blocks nothing, was refused"
    );
    check!(
        g.entered_members() == vec![b],
        "entered {:?}",
        g.entered_members()
    );
    check!(
        g.thread_pending() == 0 && signal::pending(b) == 0,
        "the fault left a signal pending"
    );
    teardown(&g)
}

/// A fatal default action ends the whole thread group, however it was aimed.
pub fn threadsig_fatal_default_ends_the_group() -> Result<(), String> {
    let g = group(2)?;
    task::harness::switch_current(task::KERNEL_TASK);
    signal::send_tid(
        task::KERNEL_TASK,
        g.threads[1],
        signal::SIGTERM,
        SigInfo::kernel(),
    )
    .map_err(|e| format!("{e:?}"))?;
    g.sweep();
    for slot in g.members() {
        check!(
            task::harness::state(slot) == Some(TaskState::Done),
            "member {slot} survived the group's fatal signal: {:?}",
            task::harness::state(slot)
        );
    }
    let (reaped, status) = task::reap_child().ok_or("the leader was not reapable")?;
    check!(
        reaped == g.leader && status == 128 + signal::SIGTERM as u64,
        "leader reaped as {reaped}/{status}"
    );
    reclaim_threads(&g.threads);
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// A recycled slot starts clean: the mask, pending set and alternate stack of
/// the thread that died in it do not reach the next one.
pub fn threadsig_recycled_slot_starts_clean() -> Result<(), String> {
    let mut g = group(1)?;
    let a = g.threads[0];
    signal::set_blocked(a, kbit(USR1));
    check!(tgkill(&g, g.leader, a, USR1) == 0, "tgkill");
    let stack = [STACKS, 0, SPAN];
    check!(
        as_thread(a, SIGALTSTACK, &[stack.as_ptr() as u64, 0]) == 0,
        "sigaltstack"
    );
    reclaim_threads(&[a]);
    check!(
        task::harness::state(a).is_none(),
        "the thread's slot was not reclaimed"
    );
    task::harness::switch_current(g.leader);
    let next = thread()?;
    check!(next == a, "slot {a} was not recycled (got {next})");
    check!(
        signal::blocked(a) == 0 && signal::pending_thread(a) == 0 && !signal::altstack(a).enabled,
        "the new thread inherited the dead one's state: mask {:#x}, pending {:#x}",
        signal::blocked(a),
        signal::pending_thread(a)
    );
    g.threads = vec![next];
    teardown(&g)
}
