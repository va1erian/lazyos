//! POSIX-ish signals: state, delivery, and the kernel-side frame mechanics
//! (issue #60).
//!
//! The model is deliberately small but Linux-shaped:
//!
//! * state is split the way POSIX threads split it (issue #772, [`registry`]):
//!   dispositions and the shared pending set of process-directed signals are
//!   **per process**, keyed by the address space (`pml4`; tasks that share one
//!   are threads of one process); the blocked mask, the pending set of
//!   thread-directed signals (`tkill`/`tgkill`, faults) and the alternate
//!   stack are **per thread**, keyed by the task slot. A process-directed
//!   signal (`kill`) is taken by one thread that does not block it.
//! * each signal has a [`Disposition`]: default kernel action, ignore, or a
//!   user handler. [`default_action`] classifies the standard dispositions
//!   (term/stop/cont/ignore/core), and `SIGKILL`/`SIGSTOP` are never catchable.
//! * `send` queues a pending bit and wakes the blocked thread that may take it
//!   (the target of a thread-directed signal; one eligible thread for a
//!   process-directed one) with [`WakeReason::Interrupted`] so the blocking
//!   syscall returns `EINTR` at a point where the signal can be delivered. Fatal signals (`SIGKILL`, and
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
//! * [`sweep`] runs from the scheduler for tasks whose saved frame is a
//!   user frame, covering native `int 0x80` programs (whose syscall stub is not
//!   part of this issue's scope) and Linux tasks preempted in user mode. It
//!   applies default actions to all of them, but writes a handler frame only
//!   through the installed page table, i.e. for the interrupted task's own
//!   process; every other task gets its frame from [`deliver_on_resume`] once
//!   the scheduler has installed its table (issue #375).
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

mod access;
mod consts;
mod control;
mod deliver;
mod fault;
mod frames;
pub mod harden;
mod native;
mod registry;
mod send;
mod sweep;
mod types;

pub use fault::{deliver_exception, deliver_fault, segv_code, Exception};
pub use sweep::{deliver_on_resume, finish_sweep, sweep, wait_interrupted};

pub use harden::{die_with_segv, restore_frame};
pub use native::{deliver_native, native_fatal_pending};
pub use send::{kill, kill_terminal_group, send_tid};

pub use access::*;
pub use consts::*;
pub use control::*;
pub use deliver::*;
pub use frames::*;
use registry::*;
pub use registry::{exec_thread, forget, fork_inherit, registry_locked, thread_born};
pub use types::*;

#[cfg(lazyos_tests)]
pub use registry::harness;

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
