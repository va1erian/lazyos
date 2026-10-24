//! Which CPU each task is current on (docs/smp-plan.md S8.3b).
//!
//! A task that is current on a CPU is running there: in user mode, inside a
//! syscall, or napping with the entry lock dropped (`task::nap`). Its
//! kernel stack is live there and its saved frame is stale, so no other CPU
//! may resume it, and no CPU may free its slot or its address space. With
//! one global run queue that is the whole answer to "two CPUs on one
//! stack": a parked or preempted task is current nowhere and may run
//! anywhere; a running or napping one is current exactly where it runs.
//!
//! The mark is set by `install` when a CPU switches to a task and cleared
//! when that CPU installs another, both inside the scheduler and so under
//! the entry lock. It is read under the lock too (selection, the handoff,
//! reaping), so plain relaxed atomics suffice: the lock orders them.

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use super::MAX_TASKS;
use crate::arch::percpu;

/// Per slot: the CPU it is current on, as area index + 1 (0: none).
static ON_CPU: [AtomicU8; MAX_TASKS] = [const { AtomicU8::new(0) }; MAX_TASKS];

/// This CPU's mark.
fn me() -> u8 {
    // At most `MAX_CPUS` (64) areas, so the index fits.
    percpu::current().index() as u8 + 1
}

/// This CPU switches from `prev` to `next`: `prev` is current nowhere now,
/// `next` here.
pub(super) fn switch(prev: usize, next: usize) {
    if prev != next {
        if let Some(mark) = ON_CPU.get(prev) {
            // Only this CPU's own mark: a stale `prev` (the harness's
            // `switch_current`) must not erase another CPU's claim.
            let _ = mark.compare_exchange(me(), 0, Ordering::Relaxed, Ordering::Relaxed);
        }
    }
    if let Some(mark) = ON_CPU.get(next) {
        mark.store(me(), Ordering::Relaxed);
    }
}

/// A slot is handed to a new task (`kstack::claim_slot`): it is current
/// nowhere yet, whatever its previous owner's mark said.
pub(super) fn forget(slot: usize) {
    if let Some(mark) = ON_CPU.get(slot) {
        mark.store(0, Ordering::Relaxed);
    }
    // A refused reap of the previous owner is not this task's.
    take_refused(slot);
}

/// Whether another CPU than the boot CPU runs tasks (S8.3c sets it): until
/// then nothing can be current elsewhere, and selection skips the check.
static SEVERAL: AtomicBool = AtomicBool::new(false);

/// Record that a second CPU runs tasks from now on.
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // S8.3c's AP start
pub fn several_cpus() {
    SEVERAL.store(true, Ordering::Relaxed);
}

/// Whether a CPU other than the boot CPU runs tasks.
pub fn several() -> bool {
    SEVERAL.load(Ordering::Relaxed)
}

/// Per slot: a reap was refused because the task was current on another
/// CPU; that CPU tells the waiting parent once it switched away
/// (`take_refused`).
static REFUSED: super::slotmask::SlotMask = super::slotmask::SlotMask::new();

/// Record that a reap of `slot` was refused for its mark.
pub(super) fn note_refused(slot: usize) {
    REFUSED.set(slot);
}

/// Whether a reap of `slot` was refused since it was last asked (consumed).
pub(super) fn take_refused(slot: usize) -> bool {
    let refused = REFUSED.contains(slot);
    if refused {
        REFUSED.clear(slot);
    }
    refused
}

/// Whether `slot` is current on another CPU: it may not be selected or
/// handed the CPU here, and nobody may reclaim it.
pub fn elsewhere(slot: usize) -> bool {
    if !SEVERAL.load(Ordering::Relaxed) {
        return false;
    }
    ON_CPU.get(slot).is_some_and(|mark| {
        let cpu = mark.load(Ordering::Relaxed);
        cpu != 0 && cpu != me()
    })
}

/// The CPU `slot` is current on, if any (diagnostics and the suite).
#[allow(dead_code)]
pub fn cpu_of(slot: usize) -> Option<usize> {
    match ON_CPU.get(slot)?.load(Ordering::Relaxed) {
        0 => None,
        mark => Some(usize::from(mark) - 1),
    }
}

/// Test hook: claim `slot` for CPU `cpu` (or for nobody), to stage a task
/// running on a CPU that runs no tasks yet.
#[cfg(lazyos_tests)]
pub fn set_for_test(slot: usize, cpu: Option<usize>) {
    several_cpus();
    ON_CPU[slot].store(cpu.map_or(0, |cpu| cpu as u8 + 1), Ordering::Relaxed);
}
