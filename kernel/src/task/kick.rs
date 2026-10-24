//! Placing a task on another CPU (docs/smp-plan.md S8.3c).
//!
//! The run queue is one, and every CPU picks from it at each of its
//! scheduler entries; what this adds is promptness. A task that became
//! runnable and that the local CPU will not run now (a wake that does not
//! preempt here, the task a local preemption put back, a new task) goes to
//! another CPU instead of waiting for that CPU's next tick: an idle one
//! first, else the one running the lowest class below the task's. That
//! CPU's `need_resched` is set and it is sent the kick (`arch::smp::kick`),
//! whose gate is a preemption point. A CPU already asked is not asked again.
//!
//! **Placement waits for this CPU's next preemption point.** [`place`] only
//! remembers the task (a few per CPU); [`flush`] kicks for the ones still
//! runnable and current nowhere when this CPU leaves the kernel or switches:
//! a syscall, interrupt or ring-3 fault return, a scheduler entry, a nap,
//! the idle loop. A
//! Messenger call wakes its callee and then parks the caller, whose
//! scheduler entry runs the callee here directly (`preempt::hand_off`); a
//! kick sent at the wake would only have brought another CPU to queue for
//! the entry lock and find nothing (the latency harness saw the round trip
//! double and the lock waited on 200000 times). Nothing is lost by waiting:
//! a kicked CPU needs the entry lock this CPU holds until that point anyway.
//!
//! The kernel task (slot 0, the multiplexer and, in test kernels, the
//! suite itself) runs on the boot CPU only: it runs on the boot stack, the
//! device interrupts it serves land there, and the suite's cases assume
//! their CPU does not change under them.

use core::sync::atomic::{AtomicU64, Ordering};

use super::{idle, oncpu, Task, KERNEL_TASK, MAX_TASKS, TASKS};
use crate::arch::percpu::{self, MAX_CPUS};

/// Per CPU: up to four slots to place at its next preemption point, 16 bits
/// each, [`EMPTY`] where free.
static PENDING: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(u64::MAX) }; MAX_CPUS];
/// A free lane of [`PENDING`].
const EMPTY: u64 = 0xFFFF;

/// This CPU's area index.
fn me() -> usize {
    percpu::current().index() as usize
}

/// Whether CPU `cpu` may run `slot` at all.
fn may_run(slot: usize, cpu: usize) -> bool {
    (slot != KERNEL_TASK || cpu == 0) && !(cpu != 0 && idle::paused())
}

/// Whether this CPU may run `slot` (selection and the local wake rule ask).
pub(super) fn may_run_here(slot: usize) -> bool {
    may_run(slot, me())
}

/// Find `woken` a CPU (see the module docs) and remember it for this CPU's
/// next preemption point. Call with the task table held. Returns whether a
/// CPU would take it now.
pub(super) fn place(tasks: &[Option<Task>; MAX_TASKS], woken: usize) -> bool {
    if !oncpu::several() || choose(tasks, woken).is_none() {
        return false;
    }
    if !remember(woken) {
        // No lane free: place it now rather than drop it.
        if let Some(cpu) = choose(tasks, woken) {
            kick(cpu);
        }
    }
    true
}

/// The CPU that should take `woken` now, if any. Call with the table held.
fn choose(tasks: &[Option<Task>; MAX_TASKS], woken: usize) -> Option<usize> {
    let me = me();
    if let Some(cpu) = oncpu::cpu_of(woken) {
        // Current somewhere already (napping there): that CPU looks again.
        return (cpu != me).then_some(cpu);
    }
    let (rank, pass) = tasks
        .get(woken)
        .and_then(|task| task.as_ref())
        .map(|task| (task.class.rank(), task.pass))?;
    // The kernel task can only go to the boot CPU: ask it unless it runs
    // something of a higher class (the same class competes by stride there,
    // as a local wake would).
    let pinned = woken == KERNEL_TASK;
    let mut lowest: Option<(u8, usize)> = None;
    for cpu in 0..idle::scheduling_end() {
        if cpu == me || !idle::scheduling(cpu) || !may_run(woken, cpu) {
            continue;
        }
        let Some(area) = percpu::area(cpu) else {
            continue;
        };
        if area.preempt.need_resched.load(Ordering::Relaxed) {
            continue;
        }
        let running = area.current.load(Ordering::Relaxed) as usize;
        if idle::is_idle(running) {
            return Some(cpu);
        }
        let theirs = tasks
            .get(running)
            .and_then(|task| task.as_ref())
            .map(|task| (task.class.rank(), task.pass));
        if let Some((theirs, their_pass)) = theirs {
            // Same class: only where the woken task would win the pick (its
            // pass is behind), or the kick would re-select the running task
            // and charge it a quantum for nothing.
            let below = theirs < rank || (pinned && theirs == rank && pass < their_pass);
            if below && lowest.is_none_or(|(low, _)| theirs < low) {
                lowest = Some((theirs, cpu));
            }
        }
    }
    lowest.map(|(_, cpu)| cpu)
}

/// Remember `slot` in a free lane of this CPU's [`PENDING`]; false when all
/// four are taken. Only this CPU touches its word (with interrupts off).
fn remember(slot: usize) -> bool {
    let pending = &PENDING[me()];
    let word = pending.load(Ordering::Relaxed);
    for lane in 0..4 {
        let shift = lane * 16;
        let held = (word >> shift) & 0xFFFF;
        if held == slot as u64 {
            return true;
        }
        if held == EMPTY {
            let word = (word & !(0xFFFF << shift)) | ((slot as u64) << shift);
            pending.store(word, Ordering::Relaxed);
            return true;
        }
    }
    false
}

/// Kick for every remembered task that is still runnable and current
/// nowhere (one that this CPU switched to, or that another CPU took, needs
/// nothing). Call with the task table held.
pub(super) fn flush(tasks: &[Option<Task>; MAX_TASKS]) {
    let word = PENDING[me()].swap(u64::MAX, Ordering::Relaxed);
    if word == u64::MAX {
        return;
    }
    for lane in 0..4 {
        let slot = ((word >> (lane * 16)) & 0xFFFF) as usize;
        if slot as u64 == EMPTY || oncpu::cpu_of(slot).is_some() {
            continue;
        }
        if !super::sched::runnable(tasks, slot) {
            continue;
        }
        if let Some(cpu) = choose(tasks, slot) {
            kick(cpu);
        }
    }
}

/// [`flush`] from a preemption point: interrupts off, no lock held. Takes
/// the table only when something is remembered, and never when it is held
/// (then the entries wait for the next point rather than spin on it).
pub(crate) fn flush_unlocked() {
    if PENDING[me()].load(Ordering::Relaxed) == u64::MAX || TASKS.is_locked() {
        return;
    }
    flush(&TASKS.lock());
}

/// `slot`'s state changed under it (it finished, or a signal waits for it)
/// while it may be current on another CPU: that CPU looks again now (it
/// must switch away from a finished task before it can be reaped, and its
/// own sweep delivers the signal).
pub(super) fn nudge(slot: usize) {
    if let Some(cpu) = oncpu::cpu_of(slot) {
        if cpu != me() {
            kick(cpu);
        }
    }
}

/// Ask CPU `cpu` to reschedule now. Only a scheduling CPU: the kick's gate
/// takes the entry lock, which an AP still idling in `ap_entry` (holding
/// nothing, unmarked) must never be entered by.
fn kick(cpu: usize) {
    if !idle::scheduling(cpu) {
        return;
    }
    if let Some(area) = percpu::area(cpu) {
        area.preempt.need_resched.store(true, Ordering::Relaxed);
        crate::arch::smp::kick(cpu);
    }
}
