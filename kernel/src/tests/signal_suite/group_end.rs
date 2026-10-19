//! Ending and stopping a thread group (audit H5, M-T1): a thread parked
//! inside the kernel may hold a lock (an ext2 volume's gate) or a request
//! slot, so `exit_group`, a user fault and the OOM killer must not mark it
//! `Done` where it sleeps, and a stop must not overwrite its wait. It is
//! woken to unwind and ends (or stops) once it is back at a user frame, after
//! it released what it held.

use super::*;
use crate::task::wait::WaitQueue;

/// A process of a leader and one thread. The thread's saved frame is a
/// kernel frame (it is "inside a syscall"); returns `(leader, thread,
/// user_cs)`, `user_cs` being the code segment to restore when the thread
/// "returns to user mode". The leader is the current task, a child of a
/// process of its own (the kernel task's children are reclaimed by the
/// scheduler, with no status to read); `parent` is that process.
fn process_with_thread_in_kernel(parent: usize) -> Result<(usize, usize, u64), String> {
    task::harness::switch_current(parent);
    let leader = task::spawn_fork().map_err(|error| format!("leader: {error}"))?;
    task::harness::set_kind(leader, task::Kind::Native);
    task::harness::switch_current(leader);
    let thread = task::spawn_thread("group-thread", process::USER_STACK_TOP, 0, 0)
        .map_err(|error| format!("thread: {error}"))?;
    task::harness::set_kind(thread, task::Kind::Native);
    let user_cs = task::harness::set_kernel_frame(thread).ok_or("the thread has no frame")?;
    Ok((leader, thread, user_cs))
}

/// How a group ends in [`group_end_round`].
#[derive(Clone, Copy)]
enum End {
    ExitGroup(u64),
    Fault,
}

/// One round: the thread holds `gate` and sleeps on `queue`; the leader ends
/// the group; the thread must still be alive, woken and with the gate intact
/// until it leaves the kernel, and then ends with the group's status.
fn group_end_round(queue: &WaitQueue, end: End, kill_after: bool) -> Result<(), String> {
    let gate = spin::Mutex::new(());
    let parent = task::spawn_fork().map_err(|error| format!("parent: {error}"))?;
    let (leader, thread, user_cs) = process_with_thread_in_kernel(parent)?;
    let held = gate.lock();
    queue.park(thread, None);

    task::harness::switch_current(leader);
    let status = match end {
        End::ExitGroup(code) => {
            task::exit_thread_group(code);
            code
        }
        End::Fault => {
            let pml4 = task::harness::pml4(leader).ok_or("no address space")?;
            let status = 128 + u64::from(signal::SIGSEGV);
            signal::terminate_process(pml4, status);
            status
        }
    };
    check!(
        task::harness::state(leader) == Some(TaskState::Done),
        "the ending thread is {:?}",
        task::harness::state(leader)
    );
    check!(
        task::harness::state(thread) == Some(TaskState::Runnable),
        "the thread inside the kernel was ended where it slept: {:?}",
        task::harness::state(thread)
    );
    check!(
        task::harness::take_wake_reason(thread) == Some(WakeReason::Interrupted),
        "the sleeping thread was not woken to unwind"
    );
    // A `SIGKILL` that arrives while the thread unwinds joins the group exit
    // and neither changes its status nor turns an exit status into a signal
    // death (it used to underflow `status - 128` for `exit_group(0)`).
    if kill_after {
        task::harness::switch_current(task::KERNEL_TASK);
        send(thread, signal::SIGKILL)?;
        task::harness::switch_current(leader);
    }
    // Unwinding: a further wait returns at once instead of parking, and the
    // woken wait leaves the queue (stood in for by the targeted removal,
    // which must not wake it a second time).
    check!(
        queue.wait(thread, None) == WakeReason::Interrupted,
        "a thread of an ended group parked again"
    );
    check!(!queue.notify_task(thread), "the thread was woken twice");
    // Still inside the kernel: the scheduler's boundaries leave it alone.
    check!(
        !task::harness::resume_delivery(thread),
        "resume delivery ended a thread with a kernel frame"
    );
    check!(gate.try_lock().is_none(), "the gate was released for it");

    // It unwinds, dropping the gate, and dies at the user frame.
    drop(held);
    check!(gate.try_lock().is_some(), "the gate is still held");
    task::harness::set_frame_cs(thread, user_cs);
    check!(
        task::harness::resume_delivery(thread),
        "a thread of an ended group was resumed in user mode"
    );
    check!(
        task::harness::state(thread) == Some(TaskState::Done),
        "the thread survived: {:?}",
        task::harness::state(thread)
    );
    // The scheduler frees the thread's slot once it has switched away from it.
    task::harness::switch_current(thread);
    task::harness::simulate_tick();
    task::reclaim_pending();
    task::harness::switch_current(parent);
    // wait4 reports an exit status as one, and only a signal death as a signal.
    let want_signal = match end {
        End::ExitGroup(_) => 0,
        End::Fault => signal::SIGSEGV,
    };
    let reaped = task::reap_child_filtered(task::ChildFilter::Any);
    check!(
        reaped == Some((leader, status, want_signal)),
        "the group reported {reaped:?}, not {status} with signal {want_signal} (leader {:?}, thread {:?})",
        task::harness::state(leader),
        task::harness::state(thread)
    );
    check!(task::reap_child().is_none(), "an extra corpse");
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::finish(parent, 0);
    check!(task::reap_child().is_some(), "the parent was not reapable");
    Ok(())
}

/// `exit_group` ends a sibling parked inside the kernel only once it leaves
/// the kernel, and the group's status is the one `exit_group` gave.
pub fn exit_group_spares_threads_inside_the_kernel() -> Result<(), String> {
    fresh()?;
    let queue = WaitQueue::new(WaitKind::Block);
    group_end_round(&queue, End::ExitGroup(3), false)?;
    // An exit status that looks like a signal death stays an exit status, also
    // when the unwinding thread meets a kill on its way out.
    group_end_round(&queue, End::ExitGroup(130), false)?;
    group_end_round(&queue, End::ExitGroup(130), true)?;
    group_end_round(&queue, End::ExitGroup(0), true)?;
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// A user fault (and with it the OOM killer and the hardening kill, which
/// end the process the same way) spares a thread parked inside the kernel
/// until it leaves, and the whole group dies of the fault's signal.
pub fn fault_spares_threads_inside_the_kernel() -> Result<(), String> {
    fresh()?;
    let queue = WaitQueue::new(WaitKind::Block);
    group_end_round(&queue, End::Fault, false)?;
    group_end_round(&queue, End::Fault, true)?;
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// A sibling at a user frame still ends at once (it holds nothing), beside
/// one that is inside the kernel.
pub fn group_end_finishes_user_threads_at_once() -> Result<(), String> {
    fresh()?;
    let queue = WaitQueue::new(WaitKind::Block);
    let parent = task::spawn_fork().map_err(|error| format!("parent: {error}"))?;
    let (leader, inside, user_cs) = process_with_thread_in_kernel(parent)?;
    let outside = task::spawn_thread("user-thread", process::USER_STACK_TOP, 0, 0)
        .map_err(|error| format!("thread: {error}"))?;
    queue.park(inside, None);
    queue.park(outside, None);
    task::harness::switch_current(leader);
    task::exit_thread_group(0);
    check!(
        task::harness::state(outside) == Some(TaskState::Done),
        "a thread at a user frame survived the group exit: {:?}",
        task::harness::state(outside)
    );
    check!(
        task::harness::state(inside) == Some(TaskState::Runnable),
        "the thread inside the kernel was ended: {:?}",
        task::harness::state(inside)
    );
    task::harness::set_frame_cs(inside, user_cs);
    check!(
        task::harness::resume_delivery(inside),
        "the unwound thread was resumed"
    );
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::switch_current(inside);
    task::harness::simulate_tick();
    task::reclaim_pending();
    task::harness::switch_current(outside);
    task::harness::simulate_tick();
    task::reclaim_pending();
    task::harness::switch_current(parent);
    check!(
        task::reap_child() == Some((leader, 0)),
        "the group was not reapable with status 0"
    );
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// Soak: 600 groups end with a thread inside the kernel holding a gate,
/// alternating `exit_group` and a fault; every gate is released, every corpse
/// reaped, and no queue entry, signal-registry entry or frame is left.
pub fn group_end_soak() -> Result<(), String> {
    fresh()?;
    let queue = WaitQueue::new(WaitKind::Block);
    let registry = signal::harness::registry_len();
    let before = crate::mem::frame_stats().live();
    for round in 0..600u64 {
        let end = if round % 2 == 0 {
            End::ExitGroup(round % 100)
        } else {
            End::Fault
        };
        group_end_round(&queue, end, round % 3 == 0).map_err(|e| format!("round {round}: {e}"))?;
    }
    check!(
        queue.is_empty(),
        "{} wait-queue entries leaked",
        queue.len()
    );
    check!(
        signal::harness::registry_len() <= registry,
        "signal-registry entries leaked"
    );
    task::harness::reset();
    signal::harness::reset();
    let after = crate::mem::frame_stats().live();
    check!(after <= before + 8, "frames leaked: {before} -> {after}");
    Ok(())
}

/// A stop parks the threads at a user frame and leaves a thread inside the
/// kernel alone (M-T1): its wait is not overwritten (it would keep a lock
/// until `SIGCONT`), it is woken to reach a delivery boundary, and the stop
/// stays pending for the process until `SIGCONT` clears it.
pub fn stop_leaves_threads_inside_the_kernel_alone() -> Result<(), String> {
    fresh()?;
    let queue = WaitQueue::new(WaitKind::Block);
    let parent = task::spawn_fork().map_err(|error| format!("parent: {error}"))?;
    let (leader, thread, _) = process_with_thread_in_kernel(parent)?;
    let outside = task::spawn_thread("user-thread", process::USER_STACK_TOP, 0, 0)
        .map_err(|error| format!("thread: {error}"))?;
    // Waiting for an I/O completion with a deadline: not a signal wait.
    queue.park(thread, Some(u64::MAX / 2));
    task::harness::switch_current(task::KERNEL_TASK);
    send(leader, signal::SIGSTOP)?;
    let stopped = |slot: usize| {
        matches!(
            task::harness::state(slot),
            Some(TaskState::Blocked {
                wait: WaitKind::Signal,
                ..
            })
        )
    };
    check!(
        stopped(leader) && stopped(outside),
        "the threads at a user frame were not stopped"
    );
    check!(
        !stopped(thread),
        "the stop overwrote the wait of a thread inside the kernel"
    );
    check!(
        task::harness::take_wake_reason(thread) == Some(WakeReason::Interrupted),
        "the thread inside the kernel was not woken to reach its boundary"
    );
    check!(
        signal::pending(leader) & (1 << signal::SIGSTOP) != 0,
        "the stop is not pending for the process"
    );
    send(leader, signal::SIGCONT)?;
    check!(
        signal::pending(leader) & (1 << signal::SIGSTOP) == 0,
        "SIGCONT left the stop pending"
    );
    check!(
        !stopped(leader) && !stopped(outside),
        "SIGCONT did not resume the stopped threads"
    );

    // A catchable stop reaches a thread that blocks it, too: the thread inside
    // the kernel gets the unblockable marker for its boundary. 300 rounds.
    for round in 0..300 {
        signal::set_blocked(thread, 1 << signal::SIGTSTP);
        queue.park(thread, Some(u64::MAX / 2));
        send(leader, signal::SIGTSTP)?;
        check!(
            signal::pending_thread(thread) & (1 << signal::SIGSTOP) != 0,
            "round {round}: a thread blocking SIGTSTP would run on through the stop"
        );
        check!(
            stopped(leader) && !stopped(thread),
            "round {round}: leader {:?} thread {:?}",
            task::harness::state(leader),
            task::harness::state(thread)
        );
        let _ = task::harness::take_wake_reason(thread);
        queue.notify_task(thread);
        send(leader, signal::SIGCONT)?;
        check!(
            signal::pending_thread(thread) & (1 << signal::SIGSTOP) == 0 && !stopped(leader),
            "round {round}: SIGCONT left the stop behind"
        );
    }
    task::harness::finish(thread, 0);
    task::harness::finish(outside, 0);
    task::harness::finish(leader, 0);
    while task::reap_child().is_some() {}
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

static CONTINUED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// The nap hook of a stopped task: what the user does meanwhile, `SIGCONT`.
fn send_continue() {
    let me = task::current();
    CONTINUED.store(true, core::sync::atomic::Ordering::Relaxed);
    let _ = signal::send_to_slot(me, me, signal::SIGCONT, SigInfo::user(me, signal::SI_USER));
}

/// A thread whose boundary is the native (`int 0x80`) gate stops there too
/// (`deliver_native` used to look only for fatal signals, so `kill -STOP` left
/// a native service or an xui app idle in `recv` serving), and a stray wake
/// does not resume a stopped thread; only `SIGCONT` does.
pub fn stop_reaches_the_native_boundary() -> Result<(), String> {
    fresh()?;
    for round in 0..100 {
        let slot = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
        task::harness::set_kind(slot, task::Kind::Native);
        task::harness::set_kernel_frame(slot).ok_or("no frame")?;
        // `kill -STOP` while the thread is inside the kernel: nothing parks it
        // there, the stop waits for its boundary.
        send(slot, signal::SIGSTOP)?;
        check!(
            task::harness::state(slot) == Some(TaskState::Runnable),
            "round {round}: the stop parked a thread inside the kernel"
        );
        task::harness::switch_current(slot);
        CONTINUED.store(false, core::sync::atomic::Ordering::Relaxed);
        task::harness::set_nap_hook(Some(send_continue));
        signal::deliver_native();
        task::harness::set_nap_hook(None);
        check!(
            CONTINUED.load(core::sync::atomic::Ordering::Relaxed),
            "round {round}: the native boundary never stopped the thread"
        );
        check!(
            task::harness::state(slot) == Some(TaskState::Runnable)
                && signal::pending(slot) & (1 << signal::SIGSTOP) == 0,
            "round {round}: still stopped or still pending after the continue"
        );
        task::harness::switch_current(task::KERNEL_TASK);

        task::harness::finish(slot, 0);
        while task::reap_child().is_some() {}
    }
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// A stopped thread is not resumed by a wake aimed at where it slept before
/// the stop, and init is exempt from a group kill (`kill_group` used to carry
/// that assertion).
pub fn stray_wakes_and_init() -> Result<(), String> {
    fresh()?;
    let slot = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    send(slot, signal::SIGSTOP)?;
    let stopped = |slot: usize| {
        matches!(
            task::harness::state(slot),
            Some(TaskState::Blocked {
                wait: WaitKind::Signal,
                ..
            })
        )
    };
    check!(stopped(slot), "the user-frame thread was not stopped");
    for _ in 0..50 {
        check!(
            !task::wake_task(slot) && stopped(slot),
            "a stray wake resumed a stopped thread"
        );
    }
    send(slot, signal::SIGCONT)?;
    check!(!stopped(slot), "SIGCONT did not resume the thread");
    task::harness::finish(slot, 0);
    while task::reap_child().is_some() {}

    let kernel_pml4 = task::harness::pml4(task::KERNEL_TASK).ok_or("no kernel table")?;
    check!(
        signal::terminate_process(kernel_pml4, 137) == 0
            && task::harness::state(task::KERNEL_TASK) == Some(TaskState::Runnable),
        "a group kill reached init"
    );
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}
