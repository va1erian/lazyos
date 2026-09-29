//! Sending, terminating, stopping and continuing processes.

use super::*;

/// Queue `sig` for `target` (a thread; process state is shared) and, for
/// actionable signals, wake a blocked thread so the event reaches a syscall
/// boundary. Uncachable signals act immediately.
pub fn send_to_slot(
    caller: usize,
    target: usize,
    sig: u8,
    info: SigInfo,
) -> Result<(), SignalError> {
    if sig as usize >= NSIG {
        return Err(SignalError::Invalid);
    }
    let Some((pml4, _)) = slot_info(target) else {
        return Err(SignalError::NoSuchProcess);
    };
    // Every sender funnels through here, so this is the one permission gate
    // (issue #230). `sig == 0` probes existence *and* permission, like Linux.
    if !send::may_signal(caller, target, sig) {
        return Err(SignalError::NotPermitted);
    }
    if sig == 0 {
        return Ok(());
    }
    // A signal to a zombie is dropped, like Linux.
    {
        let tasks = TASKS.lock();
        if tasks[target]
            .as_ref()
            .is_some_and(|task| task.state == TaskState::Done)
        {
            return Ok(());
        }
    }
    let info = SigInfo {
        pid: if info.pid == 0 { caller } else { info.pid },
        ..info
    };

    let disposition = with_signals(pml4, |state| state.actions[sig as usize]);
    match sig {
        SIGKILL => {
            terminate_process(pml4, 128 + sig as u64);
            return Ok(());
        }
        SIGSTOP => {
            stop_process(pml4);
            return Ok(());
        }
        SIGCONT => {
            continue_process(pml4);
            // The resume always happens; a *caught* `SIGCONT` then also runs
            // its handler, which is why the default case stops here.
            if !matches!(disposition, Disposition::Handler { .. }) {
                return Ok(());
            }
        }
        _ => {}
    }

    let ignored = match disposition {
        Disposition::Ignore => true,
        Disposition::Default => default_action(sig) == DefaultAction::Ignore,
        Disposition::Handler { .. } => false,
    };
    // `SIGCHLD` is recorded even under its default-ignore disposition: it is
    // the observable child-exit event, and `wait4` keeps using its own queue.
    if ignored && sig != SIGCHLD {
        return Ok(());
    }

    let blocked_now = with_signals(pml4, |state| {
        state.pending |= bit(sig);
        state.infos[sig as usize] = info;
        state.blocked & bit(sig) != 0
    });
    if blocked_now {
        return Ok(());
    }
    let stop_default =
        disposition == Disposition::Default && default_action(sig) == DefaultAction::Stop;
    if stop_default {
        stop_process(pml4);
        return Ok(());
    }
    // Only a handler (or a term/core default) can run user code, so only those
    // interrupt a blocking syscall. `SIGCHLD` with default ignore stays quiet.
    if !ignored {
        wake_blocked_threads(pml4);
    }
    Ok(())
}

/// Terminate every live task sharing `pml4`, recording the same status for
/// each. This is Linux's thread-group exit: fatal signals take the whole
/// process with them.
pub fn terminate_process(pml4: u64, status: u64) -> usize {
    let mut slots = SlotList::new();
    {
        let tasks = TASKS.lock();
        for slot in 1..MAX_TASKS {
            if tasks[slot]
                .as_ref()
                .is_some_and(|task| task.pml4 == pml4 && task.state != TaskState::Done)
            {
                slots.push(slot);
            }
        }
    }
    let mut killed = 0;
    for slot in slots.iter() {
        if process::finish(slot, status) {
            killed += 1;
        }
    }
    killed
}

/// Park every live task sharing `pml4` as stopped (`WaitKind::Signal`).
pub(super) fn stop_process(pml4: u64) {
    let mut tasks = TASKS.lock();
    for slot in 1..MAX_TASKS {
        let Some(task) = tasks[slot].as_mut() else {
            continue;
        };
        if task.pml4 == pml4 && task.state != TaskState::Done {
            task.state = TaskState::Blocked {
                wait: WaitKind::Signal,
                deadline: None,
            };
            task.wake_reason = None;
        }
    }
}

/// Resume every task sharing `pml4` that a stop signal parked. Pending stop
/// signals are discarded: a `SIGCONT` cancels them, like Linux.
pub(super) fn continue_process(pml4: u64) {
    with_signals(pml4, |state| {
        state.pending &=
            !(bit(SIGSTOP) | bit(SIGTSTP) | bit(SIGTTIN) | bit(SIGTTOU) | bit(SIGCONT));
    });
    let mut slots = SlotList::new();
    {
        let tasks = TASKS.lock();
        for slot in 1..MAX_TASKS {
            if tasks[slot].as_ref().is_some_and(|task| {
                task.pml4 == pml4
                    && matches!(
                        task.state,
                        TaskState::Blocked {
                            wait: WaitKind::Signal,
                            ..
                        }
                    )
            }) {
                slots.push(slot);
            }
        }
    }
    for slot in slots.iter() {
        wake_task_with(slot, WakeReason::Woken);
    }
}

/// Wake blocked threads of the process so their syscall (or wait loop) can see
/// the signal. Threads parked as stopped stay parked until `SIGCONT`; every
/// other queue is fine to wake directly because the waiter re-reads its state
/// through `take_wake_reason`.
pub(super) fn wake_blocked_threads(pml4: u64) {
    let mut slots = SlotList::new();
    {
        let tasks = TASKS.lock();
        for slot in 1..MAX_TASKS {
            if tasks[slot].as_ref().is_some_and(|task| {
                task.pml4 == pml4
                    && matches!(task.state, TaskState::Blocked { wait, .. } if wait != WaitKind::Signal)
            }) {
                slots.push(slot);
            }
        }
    }
    for slot in slots.iter() {
        wake_task_with(slot, WakeReason::Interrupted);
    }
}

/// Queue `SIGCHLD` for a parent and wake it only if it asked for the signal
/// (installed a handler). The pending bit is always recorded so the event is
/// observable, but a default-disposition parent is not interrupted: `wait4` is
/// notified by its own wait queue instead.
pub fn post_sigchld(parent: usize) {
    if parent == 0 || parent == KERNEL_TASK {
        return;
    }
    let Some((pml4, _)) = slot_info(parent) else {
        return;
    };
    let disposition = with_signals(pml4, |state| state.actions[SIGCHLD as usize]);
    let _ = send_to_slot(KERNEL_TASK, parent, SIGCHLD, SigInfo::kernel());
    if matches!(disposition, Disposition::Handler { .. }) {
        wake_blocked_threads(pml4);
    }
}
