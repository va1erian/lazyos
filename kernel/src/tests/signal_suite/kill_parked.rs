//! `SIGKILL` and tasks parked inside the kernel: a task whose saved frame is
//! a kernel frame is not ended where it sleeps (it may hold a lock or a
//! request slot that only its own unwinding releases); it is woken to leave
//! its syscall and dies on the way back to user mode. Tasks in user mode
//! still end at the send.

use super::*;
use crate::task::wait::WaitQueue;

/// A native child of the kernel task whose saved frame looks like a park
/// inside a syscall. Returns the slot and the user `CS` its frame had.
fn in_kernel_child() -> Result<(usize, u64), String> {
    let slot = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    task::harness::set_kind(slot, task::Kind::Native);
    let user_cs = task::harness::set_kernel_frame(slot).ok_or("the child has no frame")?;
    Ok((slot, user_cs))
}

/// The victim of a `SIGKILL` parked in the kernel holding a lock: it is
/// woken (never marked `Done` there), neither the sweep nor resume delivery
/// ends it while its frame is a kernel frame, a later stop does not park it
/// again, and it dies once it is back in user mode, after it released the
/// lock on its way out.
pub fn kill_defers_tasks_parked_in_kernel() -> Result<(), String> {
    fresh()?;
    let queue = WaitQueue::new(WaitKind::Block);
    let gate = spin::Mutex::new(());

    let (victim, user_cs) = in_kernel_child()?;
    // The victim took the gate, then parked waiting for its I/O.
    let held = gate.lock();
    queue.park(victim, None);
    // No mask holds a kill back.
    signal::set_blocked(victim, u64::MAX);
    send(victim, signal::SIGKILL)?;
    check!(
        task::harness::state(victim) == Some(TaskState::Runnable),
        "SIGKILL left the parked victim {:?} (it must be woken, not ended)",
        task::harness::state(victim)
    );
    check!(
        task::harness::take_wake_reason(victim) == Some(WakeReason::Interrupted),
        "the parked victim was not woken with Interrupted"
    );
    check!(
        signal::killed(victim) && signal::pending(victim) & (1 << signal::SIGKILL) != 0,
        "SIGKILL is not pending for the victim: {:#x}",
        signal::pending(victim)
    );

    // Still inside the kernel: the scheduler's boundaries leave it alone.
    task::harness::run_sweep();
    check!(
        !task::harness::resume_delivery(victim),
        "resume delivery ended a task with a kernel frame"
    );
    check!(
        task::harness::state(victim) == Some(TaskState::Runnable),
        "the sweep ended a task inside the kernel: {:?}",
        task::harness::state(victim)
    );
    // A stop must not park a process that is being killed.
    send(victim, signal::SIGSTOP)?;
    check!(
        task::harness::state(victim) == Some(TaskState::Runnable),
        "SIGSTOP parked a dying task: {:?}",
        task::harness::state(victim)
    );

    // The victim unwinds: its syscall drops the gate on the way out, and the
    // native gate's return finds the kill.
    drop(held);
    check!(gate.try_lock().is_some(), "the gate is still held");
    check!(
        signal::native_fatal_pending(victim) == Some(signal::SIGKILL),
        "the syscall return would not end the victim: {:?}",
        signal::native_fatal_pending(victim)
    );
    // Back in user mode (as a preemption would find it), the kill lands.
    task::harness::set_frame_cs(victim, user_cs);
    check!(
        task::harness::resume_delivery(victim),
        "a killed task in user mode was resumed"
    );
    check!(
        task::harness::state(victim) == Some(TaskState::Done),
        "the victim survived in user mode: {:?}",
        task::harness::state(victim)
    );
    check!(
        task::reap_child() == Some((victim, 128 + signal::SIGKILL as u64)),
        "the victim did not exit with 128 + SIGKILL"
    );

    // A task stopped inside the kernel (`WaitKind::Signal`) is woken too.
    let (stopped, _) = in_kernel_child()?;
    send(stopped, signal::SIGSTOP)?;
    check!(
        matches!(
            task::harness::state(stopped),
            Some(TaskState::Blocked {
                wait: WaitKind::Signal,
                ..
            })
        ),
        "SIGSTOP did not stop the task: {:?}",
        task::harness::state(stopped)
    );
    send(stopped, signal::SIGKILL)?;
    check!(
        task::harness::state(stopped) == Some(TaskState::Runnable)
            && task::harness::take_wake_reason(stopped) == Some(WakeReason::Interrupted),
        "SIGKILL did not wake the task stopped in the kernel: {:?}",
        task::harness::state(stopped)
    );

    // A task in user mode holds nothing: it ends at the send.
    let user = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    queue.park(user, None);
    send(user, signal::SIGKILL)?;
    check!(
        task::harness::state(user) == Some(TaskState::Done),
        "SIGKILL left a task with a user frame {:?}",
        task::harness::state(user)
    );

    // Killing your own process: the caller is inside the kill syscall, so it
    // dies at that syscall's return, never at the send.
    let own = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    task::harness::set_kind(own, task::Kind::Native);
    task::harness::switch_current(own);
    let sent = send(own, signal::SIGKILL);
    task::harness::switch_current(task::KERNEL_TASK);
    sent?;
    check!(
        task::harness::state(own) == Some(TaskState::Runnable),
        "a self-kill ended the caller inside its syscall: {:?}",
        task::harness::state(own)
    );
    check!(
        signal::native_fatal_pending(own) == Some(signal::SIGKILL),
        "a self-kill would return to user code"
    );

    task::harness::finish(stopped, 0);
    task::harness::finish(own, 0);
    let mut reaped = 0;
    while task::reap_child().is_some() {
        reaped += 1;
    }
    check!(reaped == 3, "init reaped {reaped} corpses, expected 3");
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// Soak: many park/kill/unwind/reap generations of a task parked inside the
/// kernel. Each one is woken (never ended in the kernel), cannot park again,
/// dies once back in user mode with `128 + SIGKILL`, and leaves no queue
/// entry, signal-registry entry or frame behind.
pub fn soak_kill_parked() -> Result<(), String> {
    fresh()?;
    let queue = WaitQueue::new(WaitKind::Block);
    let registry = signal::harness::registry_len();
    let before = crate::mem::frame_stats().live();
    for round in 0..1000u32 {
        let (victim, user_cs) = in_kernel_child().map_err(|e| format!("round {round}: {e}"))?;
        queue.park(victim, None);
        send(victim, signal::SIGKILL)?;
        check!(
            task::harness::state(victim) == Some(TaskState::Runnable)
                && task::harness::take_wake_reason(victim) == Some(WakeReason::Interrupted),
            "round {round}: the parked victim was not woken: {:?}",
            task::harness::state(victim)
        );
        // Unwinding: a further wait returns at once instead of parking, and
        // the woken wait leaves the queue (stood in for by the targeted
        // removal, which must not wake it a second time).
        check!(
            queue.wait(victim, None) == WakeReason::Interrupted,
            "round {round}: a killed task parked again"
        );
        check!(
            !queue.notify_task(victim),
            "round {round}: the victim was woken twice"
        );
        task::harness::set_frame_cs(victim, user_cs);
        check!(
            task::harness::resume_delivery(victim),
            "round {round}: a killed task in user mode was resumed"
        );
        check!(
            task::reap_child() == Some((victim, 128 + signal::SIGKILL as u64)),
            "round {round}: the victim did not exit with 128 + SIGKILL"
        );
        check!(task::reap_child().is_none(), "round {round}: extra corpse");
    }
    check!(
        queue.is_empty(),
        "{} wait-queue entries leaked",
        queue.len()
    );
    let left = signal::harness::registry_len();
    check!(
        left <= registry,
        "signal-registry entries leaked: {registry} -> {left}"
    );
    task::harness::reset();
    signal::harness::reset();
    let after = crate::mem::frame_stats().live();
    check!(after <= before + 8, "frames leaked: {before} -> {after}");
    serial_println!("TEST:task_signal_soak_kill_parked:INFO:1000 park/kill/reap cycles");
    Ok(())
}
