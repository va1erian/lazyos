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

use core::sync::atomic::{AtomicU8, Ordering};

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

/// Whether `slot` is current on another CPU: it may not be selected or
/// handed the CPU here, and nobody may reclaim it.
pub fn elsewhere(slot: usize) -> bool {
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
    ON_CPU[slot].store(cpu.map_or(0, |cpu| cpu as u8 + 1), Ordering::Relaxed);
}
