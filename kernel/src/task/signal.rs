//! POSIX-ish signals: state, delivery, and the kernel-side frame mechanics
//! (issue #60).
//!
//! The model is deliberately small but Linux-shaped:
//!
//! * state is **per process**, keyed by the address space (`pml4`). Tasks that
//!   share a PML4 are threads of one process, so a `kill(pid)` reaches all of
//!   them and one pending set serves the group. This is the documented
//!   simplification: real Linux keeps the blocked mask per thread.
//! * each signal has a [`Disposition`]: default kernel action, ignore, or a
//!   user handler. [`default_action`] classifies the standard dispositions
//!   (term/stop/cont/ignore/core), and `SIGKILL`/`SIGSTOP` are never catchable.
//! * `send` queues a pending bit and wakes a blocked thread with
//!   [`WakeReason::Interrupted`] so the blocking syscall returns `EINTR` at a
//!   point where the signal can be delivered. Fatal signals (`SIGKILL`, and
//!   defaults of term/core signals) end the whole thread group synchronously.
//! * `SIGCHLD` is queued even though its default is ignore: the pending bit is
//!   the observable record of the child event, and parents that installed a
//!   handler are woken for it.
//!
//! Delivery happens at the two boundaries back to ring 3:
//!
//! * `deliver_linux` runs at the end of `process::linux::linux_dispatch`, on
//!   the way out of every Linux syscall. It rebuilds the signal frame from the
//!   context captured at `syscall` entry (plus the syscall result, which is the
//!   value `rt_sigreturn` must restore when the handler returns).
//! * [`sweep`] runs from the timer scheduler for tasks whose saved frame is a
//!   user frame, covering native `int 0x80` programs (whose syscall stub is not
//!   part of this issue's scope) and Linux tasks preempted in user mode. It
//!   applies default actions and delivers handler frames in place.
//!
//! Linux frames follow `struct rt_sigframe` exactly (restorer pointer,
//! `ucontext_t`, `siginfo_t`); `rt_sigreturn` parses the same layout back.

use alloc::vec::Vec;
use spin::Mutex;

use super::{
    current, process, wake_task_with, Kind, Task, TaskState, WaitKind, WakeReason, KERNEL_TASK,
    MAX_TASKS, NEEDS_REDRAW, TASKS,
};
use crate::user_ptr;

mod consts;
mod control;
mod deliver;
mod fault;
mod frames;
pub mod harden;
mod send;
mod types;

pub use fault::{deliver_exception, deliver_fault, Exception};

pub use harden::{die_with_segv, restore_frame};
pub use send::{kill, send_tid};

pub use consts::*;
pub use control::*;
pub use deliver::*;
pub use frames::*;
pub use types::*;

/// Stride of one process's signal state, fixed by the array above.
struct Signals {
    pml4: u64,
    pending: u64,
    blocked: u64,
    actions: [Disposition; NSIG],
    infos: [SigInfo; NSIG],
    altstack: AltStack,
    /// True while a handler delivered with `SA_ONSTACK` runs, so a nested
    /// signal reuses the current stack instead of re-entering the alt stack.
    on_altstack: bool,
    /// `(task slot, mask)` `rt_sigsuspend` replaced: restored when the handler it
    /// woke for returns (it becomes that frame's saved mask), or at once when no
    /// handler ran. Only the suspending task consumes it, so a sibling thread's
    /// unrelated syscall cannot end another thread's suspend.
    suspend_restore: Option<(usize, u64)>,
}

impl Signals {
    fn new(pml4: u64) -> Self {
        Signals {
            pml4,
            pending: 0,
            blocked: 0,
            actions: [Disposition::Default; NSIG],
            infos: [SigInfo::user(0, SI_USER); NSIG],
            altstack: AltStack::DISABLED,
            on_altstack: false,
            suspend_restore: None,
        }
    }
}

/// Process signal state, keyed by address space. A process's PML4 is created at
/// `spawn`/`fork`, freed at reap/exec, and never changes while live, so the key
/// is stable exactly as long as the process is.
static SIGNALS: Mutex<Vec<Signals>> = Mutex::new(Vec::new());

/// Bit for `sig` in a 64-bit set.
const fn bit(sig: u8) -> u64 {
    1u64 << sig
}

/// Translate a Linux `sigset_t` into the kernel's internal mask.
///
/// Linux numbers the bit of signal `sig` as `1 << (sig - 1)`, while this
/// module indexes its `u64` mask directly by signal number (`1 << sig`, see
/// [`bit`]). Signal 64 (`SIGRTMAX`) would need bit 64, which does not fit in
/// the `u64`: it is dropped rather than shifted out of range. Bits above the
/// kernel's 1..=63 range likewise cannot be represented and vanish.
pub const fn linux_sigset_to_kernel(linux: u64) -> u64 {
    linux << 1
}

/// Translate the kernel's internal mask into a Linux `sigset_t` bit order.
///
/// The inverse of [`linux_sigset_to_kernel`]: kernel bit `sig` becomes Linux
/// bit `sig - 1`. Bit 0 (signal 0, "no signal") has no Linux slot and is
/// dropped; Linux bit 63 (`SIGRTMAX`) can never be produced because the
/// kernel mask has no bit 64.
pub const fn kernel_to_linux_sigset(kernel: u64) -> u64 {
    kernel >> 1
}

/// All signals that can never be blocked or caught.
const UNCATCHABLE: u64 = bit(SIGKILL) | bit(SIGSTOP);

/// A fixed-capacity slot list. Signal paths run in IRQ and scheduler context,
/// where the heap lock may be held by the preempted task, so they must not
/// allocate; `MAX_TASKS` is small enough to collect into a stack array.
struct SlotList {
    slots: [usize; MAX_TASKS],
    len: usize,
}

impl SlotList {
    fn new() -> Self {
        SlotList {
            slots: [0; MAX_TASKS],
            len: 0,
        }
    }

    fn push(&mut self, slot: usize) {
        self.slots[self.len] = slot;
        self.len += 1;
    }

    fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.slots[..self.len].iter().copied()
    }
}

/// The pending set with the uncatchable bits removed (defensive).
const fn clean_mask(mask: u64) -> u64 {
    mask & !UNCATCHABLE
}

/// Lowest numbered signal set in `mask`, ignoring bit 0 (signal 0 is "no
/// signal").
fn lowest_signal(mask: u64) -> Option<u8> {
    let set = mask & !1;
    if set == 0 {
        None
    } else {
        Some(set.trailing_zeros() as u8)
    }
}

/// Run `f` with the signal state of `pml4`, creating it on first use.
fn with_signals<R>(pml4: u64, f: impl FnOnce(&mut Signals) -> R) -> R {
    let mut all = SIGNALS.lock();
    let index = match all.iter().position(|state| state.pml4 == pml4) {
        Some(index) => index,
        None => {
            all.push(Signals::new(pml4));
            all.len() - 1
        }
    };
    f(&mut all[index])
}

/// Drop a torn-down address space's signal state.
pub fn forget(pml4: u64) {
    SIGNALS.lock().retain(|state| state.pml4 != pml4);
}

/// Inherit signal state across `fork`: dispositions, blocked mask and alt stack
/// copy over; a fresh process starts with nothing pending. Ignored signals stay
/// ignored, exactly like Linux; installed handlers are inherited too.
pub fn fork_inherit(parent: u64, child: u64) {
    let mut all = SIGNALS.lock();
    let inherited = match all.iter().find(|state| state.pml4 == parent) {
        Some(state) => {
            let mut copy = Signals::new(child);
            copy.blocked = state.blocked;
            copy.actions = state.actions;
            copy.altstack = state.altstack;
            copy
        }
        None => Signals::new(child),
    };
    all.retain(|state| state.pml4 != child);
    all.push(inherited);
}

/// The `(pml4, kind)` of a live slot.
fn slot_info(slot: usize) -> Option<(u64, Kind)> {
    let tasks = TASKS.lock();
    tasks
        .get(slot)
        .and_then(|task| task.as_ref())
        .map(|task| (task.pml4, task.kind))
}

/// The `(slot, pml4)` of the current task.
fn current_info() -> Option<(usize, u64)> {
    let slot = current();
    slot_info(slot).map(|(pml4, _)| (slot, pml4))
}

/// Dispositions and masks for the Linux shim (`rt_sigaction`/`procmask`/
/// `sigaltstack`). All of these resolve the slot to its process first, then
/// touch only the signal registry: the lock order is always task table ->
/// signal registry, never the reverse.
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

pub fn blocked(slot: usize) -> u64 {
    let Some((pml4, _)) = slot_info(slot) else {
        return 0;
    };
    with_signals(pml4, |state| state.blocked)
}

pub fn set_blocked(slot: usize, mask: u64) {
    let Some((pml4, _)) = slot_info(slot) else {
        return;
    };
    with_signals(pml4, |state| state.blocked = clean_mask(mask));
}

/// Start an `rt_sigsuspend`: remember the current mask (restored after the
/// handler it wakes for) and install `temp` in its place.
pub fn suspend_begin(slot: usize, temp: u64) {
    let Some((pml4, _)) = slot_info(slot) else {
        return;
    };
    with_signals(pml4, |state| {
        // A nested suspend keeps the outermost mask.
        if state.suspend_restore.is_none() {
            state.suspend_restore = Some((slot, state.blocked));
        }
        state.blocked = clean_mask(temp);
    });
}

/// End a suspend that no handler frame consumed: put the replaced mask back.
/// A no-op when none is outstanding. (The syscall path uses
/// [`suspend_end_for`] from `deliver_linux`; this slot-keyed form is the tests'.)
#[cfg(lazyos_tests)]
pub fn suspend_end(slot: usize) {
    let Some((pml4, _)) = slot_info(slot) else {
        return;
    };
    suspend_end_for(pml4, slot);
}

/// Test view of a sibling thread's syscall return: try to end `slot`'s suspend
/// as if task `other` (sharing its address space) had run `deliver_linux`.
#[cfg(lazyos_tests)]
pub fn suspend_end_as_other(slot: usize, other: usize) {
    let Some((pml4, _)) = slot_info(slot) else {
        return;
    };
    suspend_end_for(pml4, other);
}

/// [`suspend_end`] for a task already identified by its address space: only the
/// suspend that `slot` began is ended.
pub(super) fn suspend_end_for(pml4: u64, slot: usize) {
    with_signals(pml4, |state| {
        if let Some((owner, mask)) = state.suspend_restore {
            if owner == slot {
                state.suspend_restore = None;
                state.blocked = clean_mask(mask);
            }
        }
    });
}

/// Whether `rt_sigsuspend` may return: a signal the current mask lets through
/// is pending *and* its action is not "ignore" (POSIX: the call returns only
/// after a handler runs or the process terminates). Pending signals that would
/// be ignored are discarded here so they cannot end the wait.
pub fn suspend_wake_ready(slot: usize) -> bool {
    let Some((pml4, _)) = slot_info(slot) else {
        return false;
    };
    with_signals(pml4, |state| loop {
        let ready = state.pending & !state.blocked;
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
    })
}

pub fn altstack(slot: usize) -> AltStack {
    let Some((pml4, _)) = slot_info(slot) else {
        return AltStack::DISABLED;
    };
    with_signals(pml4, |state| state.altstack)
}

/// Install an alternate stack. Returns [`SignalError::Invalid`] for a stack
/// Linux would reject: `SS_DISABLE` is always fine, otherwise the range must be
/// non-empty, at least [`MINSIGSTKSZ`] bytes, and lie wholly in user space.
pub fn set_altstack(slot: usize, stack: AltStack) -> Result<(), SignalError> {
    if stack.enabled
        && (stack.sp == 0
            || stack.size < MINSIGSTKSZ
            || harden::altstack_top(stack.sp, stack.size).is_none())
    {
        return Err(SignalError::Invalid);
    }
    let Some((pml4, _)) = slot_info(slot) else {
        return Err(SignalError::NoSuchProcess);
    };
    with_signals(pml4, |state| state.altstack = stack);
    Ok(())
}

#[allow(dead_code)] // introspection used by the in-kernel tests
pub fn pending(slot: usize) -> u64 {
    let Some((pml4, _)) = slot_info(slot) else {
        return 0;
    };
    with_signals(pml4, |state| state.pending)
}

/// The thread group id of a slot: Linux reports the leader's pid, and LazyOS
/// pids are slots, so the group is identified by the lowest occupied slot that
/// shares the address space (the leader spawned before any thread).
pub fn tgid_of(slot: usize) -> usize {
    let Some((pml4, _)) = slot_info(slot) else {
        return slot;
    };
    let tasks = TASKS.lock();
    (0..MAX_TASKS)
        .find(|&index| tasks[index].as_ref().is_some_and(|task| task.pml4 == pml4))
        .unwrap_or(slot)
}

/// Test-harness hooks (issue #62): reset the registry between tests.
#[cfg(lazyos_tests)]
pub mod harness {
    /// Clear all process signal state.
    pub fn reset() {
        super::SIGNALS.lock().clear();
    }

    /// Number of process entries (leak check for tests).
    #[allow(dead_code)] // kept for a future registry leak test
    pub fn registry_len() -> usize {
        super::SIGNALS.lock().len()
    }
}
