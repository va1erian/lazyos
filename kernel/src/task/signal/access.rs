//! Slot-keyed accessors to signal state for the Linux shim
//! (`rt_sigaction`/`rt_sigprocmask`/`sigaltstack`/`rt_sigpending`) and the
//! kernel's waits. A slot names a *thread*: dispositions resolve to its
//! process, the blocked mask, the pending sets and the alternate stack to the
//! thread itself ([`registry`](super::registry)). The lock order is always task
//! table -> signal tables, never the reverse.

use super::*;

pub fn action(slot: usize, sig: u8) -> Disposition {
    let Some((pml4, _)) = slot_info(slot) else {
        return Disposition::Default;
    };
    with_signals(pml4, |state| state.actions[sig as usize])
}

pub fn set_action(slot: usize, sig: u8, disposition: Disposition) -> Result<(), SignalError> {
    if sig as usize >= NSIG {
        return Err(SignalError::Invalid);
    }
    if sig == SIGKILL || sig == SIGSTOP {
        return Err(SignalError::Invalid);
    }
    if let Disposition::Handler {
        handler, restorer, ..
    } = disposition
    {
        if handler != SIG_DFL && handler != SIG_IGN && restorer == 0 {
            // musl always passes a restorer; without one the handler could not
            // return into `rt_sigreturn`, and we do not map a vsyscall page.
            return Err(SignalError::Invalid);
        }
    }
    let Some((pml4, _)) = slot_info(slot) else {
        return Err(SignalError::NoSuchProcess);
    };
    with_signals(pml4, |state| state.actions[sig as usize] = disposition);
    Ok(())
}

/// The blocked mask of thread `slot`.
pub fn blocked(slot: usize) -> u64 {
    if slot >= MAX_TASKS {
        return 0;
    }
    with_thread(slot, |thread| thread.blocked)
}

/// Replace the blocked mask of thread `slot` (the uncatchable pair is dropped).
pub fn set_blocked(slot: usize, mask: u64) {
    if slot >= MAX_TASKS {
        return;
    }
    with_thread(slot, |thread| thread.blocked = clean_mask(mask));
}

/// Start an `rt_sigsuspend`: remember the current mask (restored after the
/// handler it wakes for) and install `temp` in its place.
pub fn suspend_begin(slot: usize, temp: u64) {
    with_thread(slot, |thread| {
        // A nested suspend keeps the outermost mask.
        if thread.suspend_restore.is_none() {
            thread.suspend_restore = Some(thread.blocked);
        }
        thread.blocked = clean_mask(temp);
    });
}

/// End a suspend that no handler frame consumed: put the replaced mask back.
/// A no-op when none is outstanding. The suspend belongs to one thread, so a
/// sibling's syscall return cannot end another thread's suspend. (The syscall
/// path is [`suspend_end_for`] from `deliver_linux`; this is the tests' name.)
#[cfg(lazyos_tests)]
pub fn suspend_end(slot: usize) {
    suspend_end_for(slot);
}

/// Test view of a sibling thread's syscall return: `other` finishing a syscall
/// ends only its own suspend, never `slot`'s.
#[cfg(lazyos_tests)]
pub fn suspend_end_as_other(_slot: usize, other: usize) {
    suspend_end_for(other);
}

/// End `slot`'s own suspend, if it has one.
pub(super) fn suspend_end_for(slot: usize) {
    with_thread(slot, |thread| {
        if let Some(mask) = thread.suspend_restore.take() {
            thread.blocked = clean_mask(mask);
        }
    });
}

/// Whether `rt_sigsuspend` may return: a signal the current mask lets through
/// is pending (for this thread or the process) *and* its action is not
/// "ignore" (POSIX: the call returns only after a handler runs or the process
/// terminates). Pending signals that would be ignored are discarded here so
/// they cannot end the wait.
pub fn suspend_wake_ready(slot: usize) -> bool {
    let Some((pml4, _)) = slot_info(slot) else {
        return false;
    };
    with_both(pml4, slot, |state, thread| loop {
        let ready = (state.pending | thread.pending) & !thread.blocked;
        let Some(sig) = lowest_signal(ready) else {
            return false;
        };
        let ignored = match state.actions[sig as usize] {
            Disposition::Ignore => true,
            Disposition::Default => matches!(
                default_action(sig),
                DefaultAction::Ignore | DefaultAction::Cont
            ),
            Disposition::Handler { .. } => false,
        };
        if !ignored {
            return true;
        }
        state.pending &= !bit(sig);
        thread.pending &= !bit(sig);
    })
}

/// The alternate signal stack of thread `slot` (it is per thread).
pub fn altstack(slot: usize) -> AltStack {
    with_thread(slot, |thread| thread.altstack)
}

/// Install an alternate stack for thread `slot`. Returns
/// [`SignalError::Invalid`] for a stack Linux would reject: `SS_DISABLE` is
/// always fine, otherwise the range must be non-empty, at least
/// [`MINSIGSTKSZ`] bytes, and lie wholly in user space.
pub fn set_altstack(slot: usize, stack: AltStack) -> Result<(), SignalError> {
    if stack.enabled
        && (stack.sp == 0
            || stack.size < MINSIGSTKSZ
            || harden::altstack_top(stack.sp, stack.size).is_none())
    {
        return Err(SignalError::Invalid);
    }
    if slot_info(slot).is_none() {
        return Err(SignalError::NoSuchProcess);
    }
    with_thread(slot, |thread| thread.altstack = stack);
    Ok(())
}

/// Signals pending for thread `slot`: its own set and its process's shared one.
#[allow(dead_code)] // introspection used by the in-kernel tests
pub fn pending(slot: usize) -> u64 {
    let Some((pml4, _)) = slot_info(slot) else {
        return 0;
    };
    with_both(pml4, slot, |state, thread| state.pending | thread.pending)
}

/// Only the thread-directed signals pending for `slot` (tests, `sigpending`).
#[allow(dead_code)] // introspection used by the in-kernel tests
pub fn pending_thread(slot: usize) -> u64 {
    with_thread(slot, |thread| thread.pending)
}

/// What `rt_sigpending` reports: signals pending for the thread (its own and
/// the process's) that its mask holds back.
pub fn pending_blocked(slot: usize) -> u64 {
    pending(slot) & blocked(slot)
}

/// Whether `slot`'s process is being killed: `SIGKILL` is pending (a kill,
/// or the group exit a fatal signal started). A task parked inside the kernel
/// is not ended where it sleeps (it may hold a lock or a request slot); it is
/// woken with [`WakeReason::Interrupted`] and dies on its way back to user
/// mode. A kernel wait that would otherwise park again after such a wake
/// checks this so the task gets there.
pub fn killed(slot: usize) -> bool {
    if slot == KERNEL_TASK {
        return false;
    }
    let Some((pml4, _)) = slot_info(slot) else {
        return false;
    };
    // A lookup, not `with_signals`: every wait asks, and must not create
    // registry entries for processes that never used signals.
    peek(|procs, _| {
        procs
            .iter()
            .find(|state| state.pml4 == pml4)
            .is_some_and(|state| state.pending & bit(SIGKILL) != 0)
    })
}

/// `execve` replaced address space `old` with `new`: a kill that was pending
/// for the old image (the task was parked loading the new one) carries over,
/// so the new image dies on its first return to user mode instead of
/// escaping the kill.
pub fn carry_kill(old: u64, new: u64) {
    let exit = peek(|procs, _| {
        procs
            .iter()
            .find(|state| state.pml4 == old && state.pending & bit(SIGKILL) != 0)
            .map(|state| state.group_exit)
    });
    let Some(exit) = exit else {
        return;
    };
    with_signals(new, |state| {
        state.pending |= bit(SIGKILL);
        if state.group_exit.is_none() {
            state.group_exit = exit;
        }
    });
}
