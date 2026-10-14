//! The scheduler's delivery boundary: default actions and handler frames for
//! tasks whose saved frame is a user frame.
//!
//! A handler frame is a write to the task's user stack, and the kernel writes
//! user memory through whatever page table is installed. The timer sweep runs
//! with the table of the task the tick interrupted, so it may only lay out a
//! frame for a task that shares that table; for any other task the frame
//! would land in the interrupted task's copy of the address (or nowhere), and
//! the target would resume at its handler with a stack holding no frame and
//! `ret` into garbage (issue #375: BusyBox `sh` dying with `rip 0x2` when a
//! child's exit posted `SIGCHLD` while the shell sat preempted). Such a task
//! is left pending and delivered by [`deliver_on_resume`], which the scheduler
//! calls once the task's own table is installed: its return to user mode,
//! exactly where Linux delivers.

use super::*;
use crate::task::trace;

/// One task the timer sweep ended, to be finished with its side effects after
/// the task table lock is dropped.
#[derive(Clone, Copy)]
pub struct SweepFinish {
    /// The task that was ended (its process dies with it on a fatal signal).
    pub slot: u16,
    /// Its parent, to receive `SIGCHLD` once the table lock is dropped.
    pub parent: u16,
    /// The exit status recorded, truncated (diagnostics). The fields are
    /// narrow on purpose: the sweep returns one of these per task slot by
    /// value in timer-interrupt context, where a kernel stack is scarce.
    #[allow(dead_code)]
    pub status: u32,
}

const _: () = assert!(MAX_TASKS <= u16::MAX as usize);

/// Terminations one [`sweep`] reports. The list is returned by value in
/// timer-interrupt context, where a kernel stack is scarce, so it holds a
/// fixed batch (1 KiB), not one entry per task slot (8 KiB at 1024): a sweep
/// that fills it stops, and the tasks it did not reach still have their
/// signal pending for the next scheduler entry.
pub const SWEEP_BATCH: usize = 128;

const NO_FINISH: SweepFinish = SweepFinish {
    slot: 0,
    parent: 0,
    status: 0,
};

/// Apply default actions to every runnable task whose saved frame is a user
/// frame, and handler frames to those whose table is `active_pml4` (the one
/// installed right now). Runs on the scheduler's lock, so it mutates the task
/// table in place and returns the terminations (with their length) for
/// post-processing after the lock is dropped.
///
/// Only runnable threads with a deliverable signal are visited (P6.1): one
/// pass over the signal tables finds them, and nothing else runs when there
/// are none, which is nearly every entry. A thread's deliverable signals are
/// its own pending set and its process's shared one, less what it blocks.
///
/// # Safety
/// `tasks` must be the live task table; each `Task::rsp` must point at an
/// interrupt frame (the scheduler stores exactly that).
pub unsafe fn sweep(
    tasks: &mut [Option<Task>; MAX_TASKS],
    active_pml4: u64,
) -> ([SweepFinish; SWEEP_BATCH], usize) {
    let mut finished = [NO_FINISH; SWEEP_BATCH];
    let mut finished_len = 0;
    let ready = deliverable_slots(tasks);
    for slot in ready.iter() {
        if finished_len == SWEEP_BATCH {
            break;
        }
        if let Some(finish) = sweep_slot(tasks, slot, active_pml4) {
            finished[finished_len] = finish;
            finished_len += 1;
        }
    }
    (finished, finished_len)
}

/// The runnable threads with a signal their mask lets through, collected
/// without allocating (scheduler context) or creating registry entries.
fn deliverable_slots(tasks: &[Option<Task>; MAX_TASKS]) -> SlotList {
    let mut ready = SlotList::new();
    let runnable = crate::task::runq::runnable_any();
    peek(|procs, threads| {
        // Nearly always nothing is waiting in any process's shared set, and
        // the scan below then only reads each runnable thread's own.
        let any_shared = procs.iter().any(|state| state.pending != 0);
        for slot in runnable.iter() {
            let Some(task) = tasks[slot].as_ref().filter(|_| slot != KERNEL_TASK) else {
                continue;
            };
            let mut pending = threads[slot].pending;
            if any_shared {
                if let Some(state) = procs.iter().find(|state| state.pml4 == task.pml4) {
                    pending |= state.pending;
                }
            }
            if pending & !clean_mask(threads[slot].blocked) != 0 {
                ready.push(slot);
            }
        }
    });
    ready
}

/// Whether thread `slot` of process `pml4` has a signal its mask lets through
/// (a lookup: it never creates the registry entry `with_signals` would).
fn has_deliverable(pml4: u64, slot: usize) -> bool {
    peek(|procs, threads| {
        let thread = &threads[slot];
        let shared = procs
            .iter()
            .find(|state| state.pml4 == pml4)
            .map_or(0, |state| state.pending);
        (thread.pending | shared) & !clean_mask(thread.blocked) != 0
    })
}

/// Whether a wait that `slot` was woken from with `Interrupted` must end and
/// return to the syscall boundary rather than park again: its process is
/// being killed, a native task has a default-fatal signal pending, or a Linux
/// task has any signal its mask lets through (delivered, or acted on, at the
/// syscall return, as Linux interrupts a blocking call). Without it a task
/// parked in a long wait (an idle desktop client, P7) keeps a `SIGTERM`, and
/// even the `SIGKILL` that follows it, waiting until its own deadline.
pub fn wait_interrupted(slot: usize) -> bool {
    if super::killed(slot) || super::native_fatal_pending(slot).is_some() {
        return true;
    }
    let Some((pml4, kind)) = super::slot_info(slot) else {
        return false;
    };
    kind == super::Kind::Linux && has_deliverable(pml4, slot)
}

/// Deliver to the task the scheduler is about to resume, whose address space
/// is installed. Returns the termination when delivery ended the task (a
/// frame that cannot be built), which the caller must honour by picking
/// another task: a finished task is never resumed into user mode.
pub fn deliver_on_resume(slot: usize) -> Option<SweepFinish> {
    if slot == KERNEL_TASK {
        return None;
    }
    let active = crate::mem::kernel_table().as_u64();
    let mut tasks = TASKS.lock();
    // Nearly every switch: nothing pending, so skip the frame inspection.
    let pml4 = tasks[slot].as_ref()?.pml4;
    if !has_deliverable(pml4, slot) {
        return None;
    }
    // SAFETY: `tasks` is the live table and the scheduler stores an interrupt
    // frame in every `Task::rsp`.
    unsafe { sweep_slot(&mut tasks, slot, active) }
}

/// Apply the next actionable signal of one runnable task with a user frame,
/// consuming ignored ones on the way. Handler frames are written only when
/// the task's table is `active_pml4`; otherwise the signal stays pending for
/// [`deliver_on_resume`]. Returns the termination if the task was ended.
///
/// # Safety
/// As [`sweep`].
unsafe fn sweep_slot(
    tasks: &mut [Option<Task>; MAX_TASKS],
    slot: usize,
    active_pml4: u64,
) -> Option<SweepFinish> {
    let (pml4, kind, rsp) = {
        let task = tasks[slot].as_ref()?;
        if task.state != TaskState::Runnable {
            return None;
        }
        (task.pml4, task.kind, task.rsp)
    };
    // Only a frame saved from ring 3 can take a user handler; a task parked
    // inside the kernel (woken wait) is delivered by its syscall return.
    if !frame_is_user(rsp, FRAME_RIP_INDEX) {
        return None;
    }
    while let Some((sig, disposition)) = next_deliverable(pml4, slot) {
        match disposition {
            Disposition::Ignore => clear_pending(pml4, slot, sig),
            Disposition::Default => match default_action(sig) {
                DefaultAction::Ignore | DefaultAction::Cont => clear_pending(pml4, slot, sig),
                DefaultAction::Stop => {
                    // INVARIANT: `tasks[slot]` was `Some` above and `tasks`
                    // is held exclusively for the whole call, so the slot is
                    // still occupied here. Same single-CPU caveat as the
                    // scheduler unwraps in `task/mod.rs`.
                    #[allow(clippy::unwrap_used)]
                    let task = tasks[slot].as_mut().unwrap();
                    task.state = TaskState::Blocked {
                        wait: WaitKind::Signal,
                        deadline: None,
                    };
                    task.wake_reason = None;
                    crate::task::runq::sync(tasks, slot);
                    return None;
                }
                DefaultAction::Term | DefaultAction::Core => {
                    return finish(tasks, slot, fatal_status(pml4, sig));
                }
            },
            Disposition::Handler { .. } => {
                if pml4 != active_pml4 {
                    return None;
                }
                return enter_handler(tasks, slot, pml4, kind, rsp, sig);
            }
        }
    }
    None
}

/// Rewrite `slot`'s saved frame so it resumes in the handler for `sig`, with
/// the signal frame laid out on its user stack (through the installed table,
/// which the caller guarantees is the task's own).
///
/// # Safety
/// As [`sweep`].
unsafe fn enter_handler(
    tasks: &mut [Option<Task>; MAX_TASKS],
    slot: usize,
    pml4: u64,
    kind: Kind,
    rsp: u64,
    sig: u8,
) -> Option<SweepFinish> {
    let armed = arm_handler(pml4, slot, sig)?;
    let mut regs = regs_from_frame(rsp, FRAME_RIP_INDEX);
    let Some(result) = prepare_handler(&regs, sig, &armed, kind != Kind::Linux, slot) else {
        // No frame can be built: end the task like an unhandled `SIGSEGV`.
        return finish(tasks, slot, 128 + SIGSEGV as u64);
    };
    trace::record_signal(slot, sig, trace::Via::TimerSweep, regs.rip, regs.rsp);
    regs.rip = result.rip;
    regs.rsp = result.rsp;
    regs.rdi = sig as u64;
    if kind == Kind::Linux && armed.flags & SA_SIGINFO != 0 {
        regs.rsi = result.info;
        regs.rdx = result.ucontext;
    }
    apply_regs_to_frame(rsp, &regs, FRAME_RIP_INDEX);
    None
}

/// End `slot` under the table lock, recording the finish for the caller.
fn finish(tasks: &mut [Option<Task>; MAX_TASKS], slot: usize, status: u64) -> Option<SweepFinish> {
    if (129..=128 + 64).contains(&status) {
        crate::task::linuxstate::note_term_signal(tasks, slot, (status - 128) as u8);
    }
    process::finish_locked(tasks, slot, status).map(|parent| SweepFinish {
        slot: slot as u16,
        parent: parent as u16,
        status: status as u32,
    })
}

/// Finish a sweep's terminations: repaint, wake `wait4`, and post `SIGCHLD`.
/// Must be called with the task table lock released.
pub fn finish_sweep(finished: &[SweepFinish]) {
    if finished.is_empty() {
        return;
    }
    NEEDS_REDRAW.store(true, core::sync::atomic::Ordering::Relaxed);
    crate::task::family::deliver_parent_death();
    for done in finished {
        // A fatal signal ends the whole thread group, not the one thread the
        // sweep happened to catch (Linux's group exit): the rest follow it.
        if (129..=128 + 64).contains(&done.status) {
            if let Some((pml4, _)) = slot_info(done.slot as usize) {
                kill_process(pml4, u64::from(done.status));
            }
        }
        let parent = done.parent as usize;
        if parent != KERNEL_TASK && parent != 0 {
            crate::task::family::count_notification(parent);
            post_sigchld(parent);
            crate::task::childbell::ring(parent);
        }
    }
    crate::task::wait::CHILD_EXIT.notify_all();
}
