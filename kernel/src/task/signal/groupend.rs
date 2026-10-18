//! Ending a whole process: `SIGKILL`, `exit_group`, user faults, the OOM
//! killer and the hardening kill (split out of `control.rs`).
//!
//! One rule decides where a thread may be finished (audit H5): only at a user
//! frame, where it holds nothing of the kernel's. Everything else is woken to
//! unwind and dies at its syscall return.

use super::*;

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
    kill_threads(pml4, status, (129..=128 + 64).contains(&status)).0
}

/// [`kill_process`] with the caller saying whether `status` is a signal death
/// (`128 + signal`, reported by `wait4` as the signal) or an ordinary exit
/// status that merely looks like one (`exit_group(130)`). Also returns how
/// many threads it finished on the spot.
fn kill_threads(pml4: u64, status: u64, signaled: bool) -> (u64, usize) {
    // The first group exit decides the status and what it means; a later kill
    // (a `SIGKILL` arriving while the threads unwind from `exit_group(0)`)
    // joins it, and must not read the first one's status as a signal.
    let (status, signaled) = with_signals(pml4, |state| {
        state.pending |= bit(SIGKILL);
        if state.kill_at_ns == 0 {
            state.kill_at_ns = crate::arch::clock::monotonic_ns().max(1);
        }
        if state.group_exit.is_none() {
            state.group_exit = Some(status);
            state.group_signaled = signaled;
        }
        (state.group_exit.unwrap_or(status), state.group_signaled)
    });
    let me = current();
    let mut parents = SlotList::new();
    let mut parked = SlotList::new();
    let mut ended = 0;
    // Decide and finish under one hold of the table with interrupts off, so a
    // task judged to be in user mode cannot enter the kernel before it ends.
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut tasks = TASKS.lock();
        crate::task::family::note_group_exit(&mut tasks, pml4, status, signaled);
        for slot in 1..MAX_TASKS {
            let Some(task) = tasks[slot].as_ref() else {
                continue;
            };
            if task.pml4 != pml4 || task.state == TaskState::Done {
                continue;
            }
            // `128 + signal` is the status; `wait4` reports the signal itself,
            // for the tasks ended here and for those that end later.
            if signaled && (129..=128 + 64).contains(&status) {
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
                    ended += 1;
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
    (status, ended)
}

/// End the whole process `pml4` from inside one of its threads, or from
/// outside: every other thread by [`kill_process`]'s rule (finished where it
/// stands only at a user frame, woken to unwind otherwise), and the current
/// thread, if it belongs to the process, at once. This is the one way a
/// thread group ends other than a signal: `exit_group`, a user fault, the OOM
/// killer and the hardening kill all come here, because a thread parked
/// *inside* the kernel may hold a lock (an ext2 volume's gate) that marking it
/// `Done` would leak forever (audit H5). `signaled` says whether `status` is
/// `128 + signal`. Returns how many threads were finished immediately.
pub fn end_process(pml4: u64, status: u64, signaled: bool) -> usize {
    let (status, mut ended) = kill_threads(pml4, status, signaled);
    let me = current();
    let mine = me != KERNEL_TASK
        && TASKS.lock()[me]
            .as_ref()
            .is_some_and(|task| task.pml4 == pml4 && task.state != TaskState::Done);
    if mine && process::finish(me, status) {
        ended += 1;
    }
    ended
}

/// End the current task on a fatal signal and start the group exit for the
/// rest of its process (see [`kill_process`]). Called at a delivery boundary
/// on the way back to user mode, where the current task holds nothing.
pub(super) fn exit_group(pml4: u64, status: u64) -> ! {
    let status = kill_process(pml4, status);
    process::finish(current(), status);
    halt_forever()
}

/// Terminate the process `pml4`, recording `status` for every thread: the
/// fault paths' and the OOM killer's exit ([`end_process`]). `128 + signal`
/// is reported by `wait4` as that signal, and `PROC:EXIT` says the kernel
/// ended it.
pub fn terminate_process(pml4: u64, status: u64) -> usize {
    let signaled = (129..=128 + 64).contains(&status);
    if signaled {
        crate::task::procexit::note_signal(pml4, (status - 128) as u8, 0);
    }
    end_process(pml4, status, signaled)
}
