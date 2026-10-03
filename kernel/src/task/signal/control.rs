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
    send_checked(caller, target, sig, info, None)
}

/// The process group and session a terminal's signal is meant for. The
/// kernel sends it past every credential check, so the target is re-checked
/// at delivery: a slot whose task left the group or session, or that a new
/// address space took over since the group was looked up, does not get it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Expect {
    pub pgid: usize,
    pub sid: usize,
}

/// [`send_to_slot`], refusing (`NoSuchProcess`) a target that no longer
/// matches `expect`. The match is taken under the task-table lock together
/// with the address space the signal is then queued on, so the identity
/// checked is the identity signalled.
pub(super) fn send_checked(
    caller: usize,
    target: usize,
    sig: u8,
    info: SigInfo,
    expect: Option<Expect>,
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
        let task = tasks[target].as_ref();
        if let Some(expect) = expect {
            let same = task.is_some_and(|task| {
                task.pgid == expect.pgid && task.sid == expect.sid && task.pml4 == pml4
            });
            if !same {
                return Err(SignalError::NoSuchProcess);
            }
        }
        if task.is_some_and(|task| task.state == TaskState::Done) {
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
            kill_process(pml4, 128 + sig as u64);
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

/// Kill the process `pml4` (Linux's group exit), the way `SIGKILL` does.
///
/// A task can only be ended where it stands when its saved frame is a user
/// frame: it holds nothing of the kernel's. A task parked *inside* the kernel
/// (blocked in a syscall, stopped in a delivery boundary, or runnable mid-call)
/// may hold a lock or a request slot (an ext2 volume's gate, a block
/// provider's request), and marking it `Done` would leak them forever: it
/// never runs again to release them. Such a task gets `SIGKILL` pending
/// (which no mask holds back), is woken with [`WakeReason::Interrupted`] if
/// it sleeps, unwinds through its syscall and dies at the syscall return
/// ([`deliver_linux`], `deliver_native`) or in the sweep once its frame is a
/// user frame. The current task is always inside the kernel, so it is left
/// to its own syscall return too.
///
/// Every thread records `status` (or the status of a group exit already in
/// progress). Returns that status.
pub(super) fn kill_process(pml4: u64, status: u64) -> u64 {
    let status = with_signals(pml4, |state| {
        state.pending |= bit(SIGKILL);
        *state.group_exit.get_or_insert(status)
    });
    let me = current();
    let mut parents = SlotList::new();
    let mut parked = SlotList::new();
    // Decide and finish under one hold of the table with interrupts off, so a
    // task judged to be in user mode cannot enter the kernel before it ends.
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut tasks = TASKS.lock();
        for slot in 1..MAX_TASKS {
            let Some(task) = tasks[slot].as_ref() else {
                continue;
            };
            if task.pml4 != pml4 || task.state == TaskState::Done || slot == me {
                continue;
            }
            let blocked = matches!(task.state, TaskState::Blocked { .. });
            if frame_is_user(task.rsp, FRAME_RIP_INDEX) {
                if let Some(parent) = process::finish_locked(&mut tasks, slot, status) {
                    parents.push(parent);
                }
            } else if blocked {
                parked.push(slot);
            }
        }
    });
    for parent in parents.iter() {
        process::after_finish(parent);
    }
    // Any blocked state, a stop (`WaitKind::Signal`) included: the kill ends
    // the stop, and the woken wait sees `Interrupted`.
    for slot in parked.iter() {
        wake_task_with(slot, WakeReason::Interrupted);
    }
    status
}

/// End the current task on a fatal signal and start the group exit for the
/// rest of its process (see [`kill_process`]). Called at a delivery boundary
/// on the way back to user mode, where the current task holds nothing.
pub(super) fn exit_group(pml4: u64, status: u64) -> ! {
    let status = kill_process(pml4, status);
    process::finish(current(), status);
    halt_forever()
}

/// Terminate every live task sharing `pml4`, recording the same status for
/// each, wherever each one stands. The fault paths use it for the faulting
/// process; signal delivery uses [`kill_process`], which spares tasks parked
/// inside the kernel until they leave it.
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
    // `128 + signal` is the status; `wait4` reports the signal itself.
    if (129..=128 + 64).contains(&status) {
        let mut tasks = TASKS.lock();
        for slot in slots.iter() {
            crate::task::linuxstate::note_term_signal(&mut tasks, slot, (status - 128) as u8);
        }
    }
    for slot in slots.iter() {
        if process::finish(slot, status) {
            killed += 1;
        }
    }
    killed
}

/// Park every live task sharing `pml4` as stopped (`WaitKind::Signal`). A
/// process being killed is not stopped: that would park the tasks the kill
/// woke to unwind (Linux ignores stops once a group exit began).
pub(super) fn stop_process(pml4: u64) {
    if with_signals(pml4, |state| state.pending & bit(SIGKILL) != 0) {
        return;
    }
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
