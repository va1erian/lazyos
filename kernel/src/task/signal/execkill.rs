//! Ending the other threads of a process that `execve`s (Linux's
//! `de_thread`): the new image runs alone, as on Linux.
//!
//! This is not a group exit. Nothing is recorded for `wait4` (no
//! `group_exit`, no `note_group_exit`, no signal death): the process goes on
//! in the new image, under the same pid. Each other thread gets `SIGKILL` in
//! its *own* pending set, so the exec'ing thread, which shares the process's
//! set, is not killed with them; [`Signals::exec_by`] names that thread so a
//! victim that reaches a fatal boundary ends alone ([`finish_exec_victim`])
//! instead of starting a group exit.
//!
//! Where a victim may be finished is [`kill_process`]'s rule (audit H5): at
//! a user frame on the spot, otherwise woken to unwind and finished at its
//! syscall return or in the sweep.

use super::*;

/// Why [`begin_exec`] refused: the process is already ending (a group exit
/// or a kill is in progress), or another thread's `execve` got there first.
/// Either way the caller dies at its syscall return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecRace {
    Dying,
    OtherExec,
}

/// Start ending every other thread of process `pml4` for an `execve` by
/// thread `me`. Returns how many threads were finished on the spot; the
/// others have `SIGKILL` pending and are woken or nudged. The caller waits
/// for them with [`exec_siblings_left`].
pub fn begin_exec(pml4: u64, me: usize) -> Result<usize, ExecRace> {
    with_signals(pml4, |state| {
        if state.group_exit.is_some() || state.pending & bit(SIGKILL) != 0 {
            return Err(ExecRace::Dying);
        }
        match state.exec_by {
            0 => {
                state.exec_by = me;
                // The kill watchdog names a victim that does not unwind
                // (#858), as it would for a `SIGKILL`.
                state.kill_at_ns = crate::arch::clock::monotonic_ns().max(1);
                Ok(())
            }
            by if by == me => Ok(()),
            _ => Err(ExecRace::OtherExec),
        }
    })?;
    let mut parents = SlotList::new();
    let mut parked = SlotList::new();
    let mut ended = 0;
    // As in `kill_threads`: decide and finish under one hold of the table with
    // interrupts off, so a thread judged to be at a user frame cannot enter
    // the kernel before it ends.
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut tasks = TASKS.lock();
        for slot in 1..MAX_TASKS {
            let Some(task) = tasks[slot].as_ref() else {
                continue;
            };
            if slot == me || task.pml4 != pml4 || task.state == TaskState::Done {
                continue;
            }
            // Task table, then the thread table: the registry's lock order.
            with_thread(slot, |thread| thread.pending |= bit(SIGKILL));
            let blocked = matches!(task.state, TaskState::Blocked { .. });
            if crate::task::oncpu::elsewhere(slot) {
                // Still current on another CPU: one that has just parked
                // (blocked, not yet switched away) must be woken, or it
                // sleeps on with the kill pending; a running one is nudged.
                if blocked {
                    parked.push(slot);
                } else {
                    crate::task::kick::nudge(slot);
                }
            } else if frame_is_user(task.rsp, FRAME_RIP_INDEX) {
                if let Some(parent) = process::finish_locked(&mut tasks, slot, EXEC_KILL_STATUS) {
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
    for slot in parked.iter() {
        crate::task::wake_stopped_with(slot, WakeReason::Interrupted);
    }
    Ok(ended)
}

/// The status an exec victim records. Nobody reads a thread's status; a
/// leader's is overwritten when the process exits (`set_pml4` moves it).
const EXEC_KILL_STATUS: u64 = 128 + SIGKILL as u64;

/// How many threads of process `pml4` other than `me` have not finished, or
/// have but are still current on another CPU (S8.3c): that CPU still runs on
/// the old table, which must not be let go (or the leader moved off it) until
/// it has switched away.
pub fn exec_siblings_left(pml4: u64, me: usize) -> usize {
    let tasks = TASKS.lock();
    (1..MAX_TASKS)
        .filter(|&slot| slot != me)
        .filter(|&slot| {
            tasks[slot].as_ref().is_some_and(|task| {
                task.pml4 == pml4
                    && (task.state != TaskState::Done || crate::task::oncpu::elsewhere(slot))
            })
        })
        .count()
}

/// Whether thread `slot` of process `pml4` has a kill pending, its own (an
/// exec ending it) or its process's. Takes no task-table lock (see
/// [`creator_killed`]).
pub fn kill_pending(pml4: u64, slot: usize) -> bool {
    peek(|procs, threads| {
        threads[slot].pending & bit(SIGKILL) != 0
            || procs
                .iter()
                .find(|state| state.pml4 == pml4)
                .is_some_and(|state| state.pending & bit(SIGKILL) != 0)
    })
}

/// Whether task `slot`, about to create a thread or a process, is being
/// killed (its process, or another thread's `execve`): it makes nothing, as
/// Linux's `copy_process` refuses. Asked with the task table `tasks` held, the
/// lock the kill's scan holds too, so a new task is either seen by the scan
/// or never made.
pub fn creator_killed(tasks: &[Option<Task>; MAX_TASKS], slot: usize) -> bool {
    slot != KERNEL_TASK
        && tasks[slot]
            .as_ref()
            .is_some_and(|task| kill_pending(task.pml4, slot))
}

/// Whether thread `slot` of process `pml4` is being ended by another
/// thread's `execve` (and not by a group exit, which outranks it).
pub fn exec_victim(pml4: u64, slot: usize) -> bool {
    peek(|procs, _| {
        procs
            .iter()
            .find(|state| state.pml4 == pml4)
            .is_some_and(|state| {
                state.exec_by != 0 && state.exec_by != slot && state.group_exit.is_none()
            })
    })
}

/// End thread `slot`, an exec victim at a fatal boundary, by itself: no group
/// exit. False (nothing done) when `slot` is not one.
pub fn finish_exec_victim(slot: usize, status: u64) -> bool {
    let Some((pml4, _)) = slot_info(slot) else {
        return false;
    };
    if !exec_victim(pml4, slot) {
        return false;
    }
    process::finish(slot, status);
    true
}
