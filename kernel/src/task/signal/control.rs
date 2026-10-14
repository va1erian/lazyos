//! Sending, terminating, stopping and continuing processes.

use super::*;

/// Who a signal is aimed at, and so which pending set it waits in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    /// The process `target` belongs to (`kill`, `SIGCHLD`, the terminal): the
    /// shared set, taken by one thread that does not block it.
    Process,
    /// The thread `target` and no other (`tkill`/`tgkill`, `SIGPIPE`): its own
    /// set, taken by that thread alone, whatever its siblings block.
    Thread,
}

/// Queue `sig` for the process of `target` and, for actionable signals, wake
/// one blocked thread that may take it so the event reaches a syscall
/// boundary. Uncatchable signals act immediately.
pub fn send_to_slot(
    caller: usize,
    target: usize,
    sig: u8,
    info: SigInfo,
) -> Result<(), SignalError> {
    send_checked(caller, target, sig, info, None, Scope::Process)
}

/// Queue `sig` for thread `target` alone (`tkill`/`tgkill`, a `SIGPIPE`): it is
/// delivered on that thread and wakes that thread, no other.
pub fn send_to_thread(
    caller: usize,
    target: usize,
    sig: u8,
    info: SigInfo,
) -> Result<(), SignalError> {
    send_checked(caller, target, sig, info, None, Scope::Thread)
}

/// An interval or POSIX timer's expiry (`task::itimer`): `sig` for the
/// process of `target`, or for the thread `target` alone when
/// `thread_directed`. The timer is its own sender, so no credential check
/// applies; a signal that is ignored or blocked is handled by the usual
/// rules. `SIGKILL` and `SIGSTOP` are refused when a timer is created, never
/// reached here.
pub fn send_timer(target: usize, sig: u8, info: SigInfo, thread_directed: bool) {
    let scope = if thread_directed {
        Scope::Thread
    } else {
        Scope::Process
    };
    let _ = send_checked(target, target, sig, info, None, scope);
}

/// What a timer's expiry should do with its signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerGate {
    /// Queue it.
    Send,
    /// The same signal is already pending for the target: a second one would
    /// merge into it, so the expiry only counts as an overrun.
    Pending,
    /// Ignored (or the process is gone): nothing to queue.
    Ignored,
}

/// Decide, cheaply and without allocating, whether a timer's `sig` for
/// `target` (a thread of process `pml4`) needs the full send path. A flood
/// of expiries of a blocked or ignored signal must not walk the task table
/// each time.
pub fn timer_gate(pml4: u64, target: usize, sig: u8, thread_directed: bool) -> TimerGate {
    if sig as usize >= NSIG {
        return TimerGate::Ignored;
    }
    peek(|procs, threads| {
        let Some(state) = procs.iter().find(|state| state.pml4 == pml4) else {
            return TimerGate::Ignored;
        };
        let ignored = match state.actions[sig as usize] {
            Disposition::Ignore => true,
            Disposition::Default => default_action(sig) == DefaultAction::Ignore,
            Disposition::Handler { .. } => false,
        };
        if ignored {
            return TimerGate::Ignored;
        }
        let pending = if thread_directed {
            threads[target].pending
        } else {
            state.pending
        };
        if pending & bit(sig) != 0 {
            TimerGate::Pending
        } else {
            TimerGate::Send
        }
    })
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

/// [`send_to_slot`] / [`send_to_thread`] (by `scope`), refusing
/// (`NoSuchProcess`) a target that no longer matches `expect`. The match is
/// taken under the task-table lock together with the address space the signal
/// is then queued on, so the identity checked is the identity signalled.
pub(super) fn send_checked(
    caller: usize,
    mut target: usize,
    sig: u8,
    info: SigInfo,
    expect: Option<Expect>,
    scope: Scope,
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
    // A signal to a zombie is dropped, like Linux. A process-directed one
    // aimed at an exited thread (the group leader may leave first) goes to a
    // live thread of the same process instead.
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
            let sibling = (scope == Scope::Process)
                .then(|| live_thread_of(&tasks, pml4))
                .flatten();
            match sibling {
                Some(sibling) => target = sibling,
                None => return Ok(()),
            }
        }
    }
    let info = SigInfo {
        // A timer's `si_tid` is its id, 0 included.
        // A timer's `si_tid` is its id, 0 included, and a kernel-raised
        // `SI_KERNEL` signal has no sending process.
        pid: if info.pid == 0 && info.code != SI_TIMER && info.code != SI_KERNEL {
            caller
        } else {
            info.pid
        },
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

    let Some(taker) = queue(pml4, target, sig, info, scope) else {
        // Queued, but every thread that could take it blocks it for now.
        return Ok(());
    };
    let stop_default =
        disposition == Disposition::Default && default_action(sig) == DefaultAction::Stop;
    if stop_default {
        stop_process(pml4);
        return Ok(());
    }
    // Only a handler (or a term/core default) can run user code, so only those
    // interrupt a blocking syscall. `SIGCHLD` with default ignore stays quiet.
    if !ignored {
        wake_blocked_thread(taker);
    }
    Ok(())
}

/// A live task of address space `pml4`, if any thread of it still runs.
fn live_thread_of(tasks: &[Option<Task>; MAX_TASKS], pml4: u64) -> Option<usize> {
    (1..MAX_TASKS).find(|&slot| {
        tasks[slot]
            .as_ref()
            .is_some_and(|task| task.pml4 == pml4 && task.state != TaskState::Done)
    })
}

/// Put `sig` in the pending set `scope` names and return the thread that
/// should be woken for it: `target` itself for a thread-directed signal
/// (`None` while that thread blocks it), and for a process-directed one the
/// target thread if it may take the signal, else the first thread of the
/// process that may (`None` when all block it: it waits in the shared set
/// until one unblocks).
fn queue(pml4: u64, target: usize, sig: u8, info: SigInfo, scope: Scope) -> Option<usize> {
    match scope {
        Scope::Thread => with_thread(target, |thread| {
            thread.pending |= bit(sig);
            thread.infos[sig as usize] = info;
            (thread.blocked & bit(sig) == 0).then_some(target)
        }),
        Scope::Process => {
            with_signals(pml4, |state| {
                state.pending |= bit(sig);
                state.infos[sig as usize] = info;
            });
            eligible_thread(pml4, target, sig)
        }
    }
}

/// The thread a process-directed `sig` should go to: `preferred` if it is live
/// and does not block `sig`, else the lowest live slot of the process that does
/// not (Linux's `complete_signal`, which also tries the target thread first).
fn eligible_thread(pml4: u64, preferred: usize, sig: u8) -> Option<usize> {
    let mut slots = SlotList::new();
    let preferred_live;
    {
        let tasks = TASKS.lock();
        let live = |slot: usize| {
            tasks[slot]
                .as_ref()
                .is_some_and(|task| task.pml4 == pml4 && task.state != TaskState::Done)
        };
        preferred_live = live(preferred);
        for slot in 1..MAX_TASKS {
            if slot != preferred && live(slot) {
                slots.push(slot);
            }
        }
    }
    peek(|_, threads| {
        let takes = |slot: usize| threads[slot].blocked & bit(sig) == 0;
        if preferred_live && takes(preferred) {
            return Some(preferred);
        }
        slots.iter().find(|&slot| takes(slot))
    })
}

/// Interrupt `slot` if it is parked in a blocking call, so the call returns
/// `EINTR` at a point where the signal can be delivered on that thread. A
/// thread parked as stopped stays parked until `SIGCONT`; every other queue is
/// fine to wake directly because the waiter re-reads its state through
/// `take_wake_reason`.
pub(super) fn wake_blocked_thread(slot: usize) {
    let parked = TASKS.lock()[slot].as_ref().is_some_and(
        |task| matches!(task.state, TaskState::Blocked { wait, .. } if wait != WaitKind::Signal),
    );
    if parked {
        wake_task_with(slot, WakeReason::Interrupted);
    }
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
        crate::task::family::note_group_exit(&mut tasks, pml4, status);
        for slot in 1..MAX_TASKS {
            let Some(task) = tasks[slot].as_ref() else {
                continue;
            };
            if task.pml4 != pml4 || task.state == TaskState::Done {
                continue;
            }
            // `128 + signal` is the status; `wait4` reports the signal itself,
            // for the tasks ended here and for those that end later.
            if (129..=128 + 64).contains(&status) {
                crate::task::linuxstate::note_term_signal(&mut tasks, slot, (status - 128) as u8);
            }
            if slot == me {
                continue;
            }
            let Some(task) = tasks[slot].as_ref() else {
                continue;
            };
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
    crate::task::family::note_group_exit(&mut TASKS.lock(), pml4, status);
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
            crate::task::runq::sync(&tasks, slot);
        }
    }
}

/// Resume every task sharing `pml4` that a stop signal parked. Pending stop
/// signals are discarded: a `SIGCONT` cancels them, like Linux.
pub(super) fn continue_process(pml4: u64) {
    const STOPS: u64 =
        (1 << SIGSTOP) | (1 << SIGTSTP) | (1 << SIGTTIN) | (1 << SIGTTOU) | (1 << SIGCONT);
    with_signals(pml4, |state| state.pending &= !STOPS);
    let mut stopped = SlotList::new();
    let mut threads = SlotList::new();
    {
        let tasks = TASKS.lock();
        for slot in 1..MAX_TASKS {
            let Some(task) = tasks[slot].as_ref().filter(|task| task.pml4 == pml4) else {
                continue;
            };
            threads.push(slot);
            if matches!(
                task.state,
                TaskState::Blocked {
                    wait: WaitKind::Signal,
                    ..
                }
            ) {
                stopped.push(slot);
            }
        }
    }
    for slot in threads.iter() {
        with_thread(slot, |thread| thread.pending &= !STOPS);
    }
    for slot in stopped.iter() {
        wake_task_with(slot, WakeReason::Woken);
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
    let _ = send_to_slot(KERNEL_TASK, parent, SIGCHLD, SigInfo::kernel());
}
