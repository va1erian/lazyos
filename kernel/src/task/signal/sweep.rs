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
    /// The task that was ended (diagnostics).
    #[allow(dead_code)]
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
/// # Safety
/// `tasks` must be the live task table; each `Task::rsp` must point at an
/// interrupt frame (the scheduler stores exactly that).
pub unsafe fn sweep(
    tasks: &mut [Option<Task>; MAX_TASKS],
    active_pml4: u64,
) -> ([SweepFinish; MAX_TASKS], usize) {
    let mut finished = [NO_FINISH; MAX_TASKS];
    let mut finished_len = 0;
    for slot in 1..MAX_TASKS {
        if let Some(finish) = sweep_slot(tasks, slot, active_pml4) {
            finished[finished_len] = finish;
            finished_len += 1;
        }
    }
    (finished, finished_len)
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
    while let Some((sig, disposition)) = next_deliverable(pml4) {
        match disposition {
            Disposition::Ignore => clear_pending(pml4, sig),
            Disposition::Default => match default_action(sig) {
                DefaultAction::Ignore | DefaultAction::Cont => clear_pending(pml4, sig),
                DefaultAction::Stop => {
                    // INVARIANT: `tasks[slot]` was `Some` above and `tasks`
                    // is held exclusively for the whole call, so the slot is
                    // still occupied here. Same single-CPU caveat as the
                    // scheduler unwraps in `task/mod.rs`.
                    let task = tasks[slot].as_mut().unwrap();
                    task.state = TaskState::Blocked {
                        wait: WaitKind::Signal,
                        deadline: None,
                    };
                    task.wake_reason = None;
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
    let Some(result) = prepare_handler(&regs, sig, &armed, kind != Kind::Linux) else {
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
    for done in finished {
        let parent = done.parent as usize;
        if parent != KERNEL_TASK && parent != 0 {
            post_sigchld(parent);
            crate::task::childbell::ring(parent);
        }
    }
    crate::task::wait::CHILD_EXIT.notify_all();
}
