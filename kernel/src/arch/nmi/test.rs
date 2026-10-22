//! The NMI handler's test modes (test kernels only): what a CPU's handler
//! does instead of the hang report while the suite drives NMIs at it.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::percpu::Cpu;

/// The real hang report.
pub const REPORT: u8 = 0;
/// Count the NMI (`IpiState::nmis`) and return.
pub const QUIET: u8 = 1;
/// Once: take a fault inside the handler (its `iretq` unblocks NMIs), send
/// this CPU a second NMI and wait for the gate to drop it as nested; then
/// fall back to [`QUIET`].
pub const NESTED: u8 = 2;

/// The last [`NESTED`] round's outcome (`Outcome as u64`).
static OUTCOME: AtomicU64 = AtomicU64::new(0);
/// The address the [`NESTED`] round's fault reads (unmapped; the suite
/// checks it faults before arming).
static FAULT_AT: AtomicU64 = AtomicU64::new(0);

/// What a [`NESTED`] round saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Not run yet (or still running).
    Pending = 0,
    /// The second NMI arrived and was dropped as nested.
    Dropped = 1,
    /// The probe did not fault, so NMIs stayed blocked.
    NoFault = 2,
    /// The second NMI never showed within 10 ms.
    Missing = 3,
}

/// Set `cpu`'s mode, and the unmapped address a [`NESTED`] round faults on.
pub fn arm(cpu: &Cpu, mode: u8, fault_at: u64) {
    FAULT_AT.store(fault_at, Ordering::Relaxed);
    OUTCOME.store(Outcome::Pending as u64, Ordering::Relaxed);
    cpu.ipi.nmi_test.store(mode, Ordering::Release);
}

/// The last [`NESTED`] round's outcome.
pub fn outcome() -> Outcome {
    match OUTCOME.load(Ordering::Acquire) {
        1 => Outcome::Dropped,
        2 => Outcome::NoFault,
        3 => Outcome::Missing,
        _ => Outcome::Pending,
    }
}

/// The handler's hook: `true` when the test mode handled this NMI.
pub(super) fn intercept(cpu: &Cpu) -> bool {
    match cpu.ipi.nmi_test.load(Ordering::Acquire) {
        QUIET => true,
        NESTED => {
            cpu.ipi.nmi_test.store(QUIET, Ordering::Release);
            let outcome = nested_round();
            OUTCOME.store(outcome as u64, Ordering::Release);
            true
        }
        _ => false,
    }
}

/// The nested case, from inside the outer NMI.
fn nested_round() -> Outcome {
    let before = super::NESTED.load(Ordering::Acquire);
    if !crate::arch::probe::read_faults(FAULT_AT.load(Ordering::Relaxed)) {
        return Outcome::NoFault;
    }
    crate::arch::lapic::send_nmi(crate::arch::lapic::id());
    // SAFETY: reads the TSC; no memory or privilege effects.
    let started = unsafe { core::arch::x86_64::_rdtsc() };
    let bound = crate::arch::clock::cycles_per_tick();
    while super::NESTED.load(Ordering::Acquire) == before {
        // SAFETY: as above.
        if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(started) > bound {
            return Outcome::Missing;
        }
        core::hint::spin_loop();
    }
    Outcome::Dropped
}
