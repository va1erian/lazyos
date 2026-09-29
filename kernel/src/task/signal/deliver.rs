//! Signal delivery at the user boundary: arming handlers, default actions and the sweep.

use super::*;

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

/// The disposition changes made when a handler is entered: the mask in effect
/// while it runs, the saved mask `rt_sigreturn` restores, and the stack choice.
pub(super) struct Armed {
    pub(super) handler: u64,
    pub(super) flags: u64,
    pub(super) restorer: u64,
    pub(super) mask: u64,
    pub(super) saved_mask: u64,
    pub(super) altstack: AltStack,
    pub(super) use_altstack: bool,
    pub(super) info: SigInfo,
}

pub(super) fn next_deliverable(pml4: u64) -> Option<(u8, Disposition)> {
    with_signals(pml4, |state| {
        let ready = state.pending & !state.blocked;
        lowest_signal(ready).map(|sig| (sig, state.actions[sig as usize]))
    })
}

pub(super) fn clear_pending(pml4: u64, sig: u8) {
    with_signals(pml4, |state| state.pending &= !bit(sig));
}

/// Consume `sig` into a handler: clear it from pending, apply the action mask
/// (plus the signal itself unless `SA_NODEFER`), arm the alternate stack, and
/// honour `SA_RESETHAND`.
pub(super) fn arm_handler(pml4: u64, sig: u8) -> Option<Armed> {
    with_signals(pml4, |state| {
        let Disposition::Handler {
            handler,
            flags,
            restorer,
            mask,
        } = state.actions[sig as usize]
        else {
            return None;
        };
        // After `rt_sigsuspend` the frame must restore the mask the suspend
        // replaced, not the temporary one the handler runs under.
        let saved_mask = state.suspend_restore.take().unwrap_or(state.blocked);
        let mut next = state.blocked | mask;
        if flags & SA_NODEFER == 0 {
            next |= bit(sig);
        }
        state.blocked = clean_mask(next);
        state.pending &= !bit(sig);
        let use_altstack = flags & SA_ONSTACK != 0 && state.altstack.enabled && !state.on_altstack;
        if use_altstack {
            state.on_altstack = true;
        }
        if flags & SA_RESETHAND != 0 {
            state.actions[sig as usize] = Disposition::Default;
        }
        let info = state.infos[sig as usize];
        Some(Armed {
            handler,
            flags,
            restorer,
            mask,
            saved_mask,
            altstack: state.altstack,
            use_altstack,
            info,
        })
    })
}

/// Where a handler frame goes: the alternate stack when requested, otherwise
/// the interrupted stack. `None` for an alternate stack that wraps or leaves
/// user space (#223).
pub(super) fn handler_stack_top(regs: &UserRegs, armed: &Armed) -> Option<u64> {
    if armed.use_altstack {
        harden::altstack_top(armed.altstack.sp, armed.altstack.size)
    } else {
        Some(regs.rsp)
    }
}

/// Write a delivered signal's frame and return the handler entry context, or
/// `None` when no valid frame can be built (bad stack, non-user handler): the
/// caller then force-terminates the task with `SIGSEGV`, like Linux.
pub(super) fn prepare_handler(
    regs: &UserRegs,
    sig: u8,
    armed: &Armed,
    native: bool,
) -> Option<FrameResult> {
    let stack_top = handler_stack_top(regs, armed)?;
    // A non-canonical entry point would fault `sysretq` in ring 0.
    if !harden::is_user_addr(armed.handler) {
        return None;
    }
    if native {
        let mut result = build_native_frame(stack_top, regs, sig)?;
        result.rip = armed.handler;
        Some(result)
    } else {
        build_linux_frame(
            stack_top,
            regs,
            sig,
            armed.handler,
            armed.flags,
            armed.restorer,
            armed.mask,
            armed.saved_mask,
            &armed.info,
        )
    }
}

/// Copy a frame onto the syscall return path: `sysretq` will resume at
/// `regs.rip` with the saved general registers reloaded. The flags are
/// sanitised here, the one sink both signal delivery and `rt_sigreturn` use.
pub(crate) fn apply_linux_frame_syscall(regs: &UserRegs) {
    let rflags = harden::sanitize_rflags(regs.rflags);
    crate::arch::linux::set_user_return(regs.rip, regs.rsp, rflags);
    let saved = [
        (0usize, regs.r15),
        (1, regs.r14),
        (2, regs.r13),
        (3, regs.r12),
        (4, regs.rbp),
        (5, regs.rbx),
        (6, regs.rdi),
        (7, regs.rsi),
        (8, regs.rdx),
        (9, regs.r8),
        (10, regs.r9),
        (11, regs.r10),
    ];
    for (slot, value) in saved {
        crate::arch::linux::set_saved_register(slot, value);
    }
    // Keep the captured context in step so a nested delivery (or `clone`) sees
    // the handler's entry state rather than the syscall's.
    let context = crate::arch::linux::UserContext {
        rip: regs.rip,
        rflags,
        rsp: regs.rsp,
        rbx: regs.rbx,
        rbp: regs.rbp,
        r12: regs.r12,
        r13: regs.r13,
        r14: regs.r14,
        r15: regs.r15,
        rdi: regs.rdi,
        rsi: regs.rsi,
        rdx: regs.rdx,
        r8: regs.r8,
        r9: regs.r9,
        r10: regs.r10,
    };
    crate::arch::linux::set_user_context(context);
}

/// Rebuild the interrupted user context from this task's syscall-return stack.
///
/// The words pushed by `linux_syscall_entry` live on the *task's own* kernel
/// stack, so unlike the global `USER_CONTEXT` snapshot they cannot be
/// clobbered by another task that runs while this one is blocked inside its
/// syscall. `rax` is passed in (the syscall result the return path holds);
/// `r11`/`rcx` mirror the saved flags/rip, exactly as the `syscall` instruction
/// left them.
pub(super) fn saved_regs_from_stack(rax: u64) -> UserRegs {
    // Safety: we are inside the current task's syscall; the entry stub pushed
    // these 15 words at fixed offsets below the kernel stack top.
    let word = |slot: isize| unsafe { crate::task::sys::kernel_stack_word(slot) };
    let rsp = word(-1);
    let rflags = word(-2);
    let rip = word(-3);
    UserRegs {
        r15: word(-4),
        r14: word(-5),
        r13: word(-6),
        r12: word(-7),
        rbp: word(-8),
        rbx: word(-9),
        rdi: word(-10),
        rsi: word(-11),
        rdx: word(-12),
        r8: word(-13),
        r9: word(-14),
        r10: word(-15),
        r11: rflags,
        rcx: rip,
        rax,
        rip,
        rsp,
        rflags,
    }
}

/// Halt the CPU until the scheduler runs another task.
pub(super) fn halt_forever() -> ! {
    loop {
        x86_64::instructions::interrupts::enable();
        x86_64::instructions::hlt();
    }
}

/// Park the caller while its process is stopped, resuming on `SIGCONT`. Used
/// when a delivery boundary meets a stop default. A `SIGKILL` while stopped
/// marks the task `Done`; there is nothing to resume, so it halts like any
/// other termination (the scheduler has already moved on).
pub(super) fn wait_continued() {
    loop {
        let state = {
            let tasks = TASKS.lock();
            tasks[current()].as_ref().map(|task| task.state)
        };
        match state {
            Some(TaskState::Blocked {
                wait: WaitKind::Signal,
                ..
            }) => x86_64::instructions::interrupts::enable_and_hlt(),
            Some(TaskState::Done) | None => halt_forever(),
            _ => return,
        }
    }
}

/// Apply one disposition to the current task, returning the (possibly updated)
/// register context for any further pending signal. Stops park here until
/// `SIGCONT`; fatal defaults never return.
pub(super) fn apply_action(
    pml4: u64,
    sig: u8,
    disposition: Disposition,
    regs: &mut UserRegs,
) -> bool {
    let action = match disposition {
        Disposition::Ignore => {
            clear_pending(pml4, sig);
            return true;
        }
        Disposition::Default => default_action(sig),
        Disposition::Handler { .. } => {
            let Some(armed) = arm_handler(pml4, sig) else {
                return false;
            };
            let Some(result) = prepare_handler(regs, sig, &armed, false) else {
                die_with_segv();
            };
            regs.rip = result.rip;
            regs.rsp = result.rsp;
            regs.rdi = sig as u64;
            if armed.flags & SA_SIGINFO != 0 {
                regs.rsi = result.info;
                regs.rdx = result.ucontext;
            }
            return true;
        }
    };
    match action {
        DefaultAction::Ignore => clear_pending(pml4, sig),
        DefaultAction::Cont => clear_pending(pml4, sig),
        DefaultAction::Term | DefaultAction::Core => {
            if default_action(sig) == DefaultAction::Core {
                serial_println!("signal: task {} core-dumped on signal {sig}", current());
            }
            terminate_process(pml4, 128 + sig as u64);
            halt_forever();
        }
        DefaultAction::Stop => {
            stop_process(pml4);
            wait_continued();
        }
    }
    true
}

/// Deliver pending signals on the way out of a Linux syscall. `result` is the
/// value `sysretq` would return; it is recorded as `rax` in the frame so
/// `rt_sigreturn` resumes the caller with the syscall's outcome (typically
/// `-EINTR`).
pub fn deliver_linux(result: u64) {
    let Some((slot, pml4)) = current_info() else {
        return;
    };
    let is_linux = {
        let tasks = TASKS.lock();
        tasks[slot]
            .as_ref()
            .is_some_and(|task| task.kind == Kind::Linux && task.kstack_top != 0)
    };
    if !is_linux {
        return;
    }
    let mut regs = saved_regs_from_stack(result);
    let mut frame_written = false;
    while let Some((sig, disposition)) = next_deliverable(pml4) {
        if default_action(sig) == DefaultAction::Stop && disposition == Disposition::Default {
            stop_process(pml4);
            wait_continued();
            continue;
        }
        apply_action(pml4, sig, disposition, &mut regs);
        frame_written = true;
    }
    if frame_written {
        apply_linux_frame_syscall(&regs);
    }
    // `rt_sigsuspend` woke without a handler frame consuming its saved mask (the
    // signal's action ignored it): the original mask comes back now.
    suspend_end_for(pml4);
}

/// One task the timer sweep ended, to be finished with its side effects after
/// the task table lock is dropped.
#[derive(Clone, Copy)]
pub struct SweepFinish {
    /// The task that was ended (diagnostics).
    #[allow(dead_code)]
    pub slot: usize,
    /// Its parent, to receive `SIGCHLD` once the table lock is dropped.
    pub parent: usize,
    /// The exit status recorded (diagnostics).
    #[allow(dead_code)]
    pub status: u64,
}

pub(super) const NO_FINISH: SweepFinish = SweepFinish {
    slot: 0,
    parent: 0,
    status: 0,
};

/// Apply default actions and handler frames to every runnable task whose saved
/// frame is a user frame. Runs on the scheduler's lock, so it mutates the task
/// table in place and returns the terminations (with their length) for
/// post-processing after the lock is dropped.
///
/// # Safety
/// `tasks` must be the live task table; each `Task::rsp` must point at an
/// interrupt frame (the scheduler stores exactly that).
pub unsafe fn sweep(tasks: &mut [Option<Task>; MAX_TASKS]) -> ([SweepFinish; MAX_TASKS], usize) {
    let mut finished = [NO_FINISH; MAX_TASKS];
    let mut finished_len = 0;
    for slot in 1..MAX_TASKS {
        let (pml4, kind, rsp) = {
            let Some(task) = tasks[slot].as_ref() else {
                continue;
            };
            if task.state != TaskState::Runnable {
                continue;
            }
            (task.pml4, task.kind, task.rsp)
        };
        // Only a frame saved from ring 3 can take a user handler; a task parked
        // inside the kernel (woken wait) is delivered by its syscall return.
        if !frame_is_user(rsp, FRAME_RIP_INDEX) {
            continue;
        }
        while let Some((sig, disposition)) = next_deliverable(pml4) {
            if disposition == Disposition::Ignore {
                clear_pending(pml4, sig);
                continue;
            }
            if disposition == Disposition::Default {
                match default_action(sig) {
                    DefaultAction::Ignore | DefaultAction::Cont => {
                        clear_pending(pml4, sig);
                        continue;
                    }
                    DefaultAction::Stop => {
                        // INVARIANT: `tasks[slot]` was `Some` at the top of
                        // this iteration (line above) and `tasks` is held
                        // exclusively for the whole `sweep` call, so the slot
                        // is still occupied here. Same single-CPU caveat as
                        // the scheduler unwraps in `task/mod.rs`.
                        let task = tasks[slot].as_mut().unwrap();
                        task.state = TaskState::Blocked {
                            wait: WaitKind::Signal,
                            deadline: None,
                        };
                        task.wake_reason = None;
                        break;
                    }
                    DefaultAction::Term | DefaultAction::Core => {
                        let status = 128 + sig as u64;
                        if let Some(parent) = process::finish_locked(tasks, slot, status) {
                            finished[finished_len] = SweepFinish {
                                slot,
                                parent,
                                status,
                            };
                            finished_len += 1;
                        }
                        break;
                    }
                }
            }
            // Handler: rewrite the saved frame in place.
            let Some(armed) = arm_handler(pml4, sig) else {
                break;
            };
            let mut regs = regs_from_frame(rsp, FRAME_RIP_INDEX);
            let Some(result) = prepare_handler(&regs, sig, &armed, kind != Kind::Linux) else {
                // No frame can be built: end the task like an unhandled
                // `SIGSEGV`, under the table lock this function already holds.
                let status = 128 + SIGSEGV as u64;
                if let Some(parent) = process::finish_locked(tasks, slot, status) {
                    finished[finished_len] = SweepFinish {
                        slot,
                        parent,
                        status,
                    };
                    finished_len += 1;
                }
                break;
            };
            regs.rip = result.rip;
            regs.rsp = result.rsp;
            regs.rdi = sig as u64;
            if kind == Kind::Linux && armed.flags & SA_SIGINFO != 0 {
                regs.rsi = result.info;
                regs.rdx = result.ucontext;
            }
            apply_regs_to_frame(rsp, &regs, FRAME_RIP_INDEX);
            break;
        }
    }
    (finished, finished_len)
}

/// Finish a sweep's terminations: repaint, wake `wait4`, and post `SIGCHLD`.
/// Must be called with the task table lock released.
pub fn finish_sweep(finished: &[SweepFinish]) {
    if finished.is_empty() {
        return;
    }
    NEEDS_REDRAW.store(true, core::sync::atomic::Ordering::Relaxed);
    for done in finished {
        if done.parent != KERNEL_TASK && done.parent != 0 {
            post_sigchld(done.parent);
        }
    }
    crate::task::wait::CHILD_EXIT.notify_all();
}
