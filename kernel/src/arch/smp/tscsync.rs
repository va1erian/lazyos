//! The TSC coherence gate (docs/smp-plan.md S8.2): before an AP is allowed to
//! go online, the BSP measures how far apart the two clocks read.
//!
//! The kernel keeps one monotonic stream (`arch::clock`) and reads the TSC
//! on whichever CPU a task happens to run; an AP whose counter sits far from
//! the BSP's would make time jump when a task migrates. A skewed AP is
//! refused (`HW:SMP:TSC:SKEW`, parked), never trusted: losing a core is
//! degraded performance, a clock that runs backwards is a correctness bug.
//!
//! The measurement is a two-way exchange, repeated: the AP reads its TSC
//! (`t1`), the BSP reads its own (`tb`) after seeing that, the AP reads again
//! (`t2`) after seeing the BSP's. With coherent clocks `t1 <= tb <= t2`
//! always holds (each read is ordered after the previous by the flag the
//! other side waited on). If the AP's counter runs `K` ahead of the BSP's,
//! `tb` falls `K` short of `t1` less the latency of the hand-off, so every
//! round's violation is a lower bound on `|K|` (it can only be too small, by
//! that round's latency) and the **largest** over all rounds is the tightest.
//! A round the host delayed (a descheduled vCPU) understates the skew and the
//! quiet rounds make up for it; taking the minimum instead would let one slow
//! round hide any skew.

use core::sync::atomic::Ordering;

use super::{rdtsc, spin_loop};
use crate::arch::percpu;

/// Exchanges per AP: enough that one quiet round is certain, few enough to
/// cost microseconds.
const ROUNDS: u64 = 32;

/// The BSP's own read, ordered after the flag load that precedes it.
#[inline]
fn read_tsc_ordered() -> u64 {
    // SAFETY: `lfence` has no memory or privilege effects; it keeps `rdtsc`
    // from executing ahead of the load that observed the other side's flag.
    unsafe { core::arch::x86_64::_mm_lfence() };
    rdtsc()
}

/// The step value the BSP sets when it accepts the AP's clock. The AP must
/// not publish `online` on its own after the last round: the BSP's INIT for a
/// refused CPU can land after the AP finished, and an AP that had marked
/// itself online by then would be counted as running while parked.
const ACCEPT: u64 = 4 * ROUNDS + 1;

/// The AP's half, run on the AP before it publishes `online`; returns
/// whether the BSP accepted this CPU's clock. Bounded: if the BSP gives up
/// (and parks this CPU with INIT) this loop is simply stopped, and if the BSP
/// merely stops talking the AP gives up after a second of its own clock and
/// reports refusal (the caller halts it: a CPU nobody vouched for stays off).
pub(super) fn ap_side(cpu: &percpu::Cpu) -> bool {
    let started = rdtsc();
    let bound = crate::arch::clock::cycles_per_tick() * 100;
    let wait_for = |want: u64| -> bool {
        while cpu.ipi.sy_state.load(Ordering::Acquire) != want {
            if rdtsc().wrapping_sub(started) > bound {
                return false;
            }
            spin_loop();
        }
        true
    };
    // Visible to the BSP: from here it may start the exchange.
    cpu.ipi.sy_ready.store(true, Ordering::Release);
    // Test rig: a deliberate offset, to prove a skewed AP is refused.
    #[cfg(lazyos_tests)]
    let bias = cpu.ipi.tsc_bias.load(Ordering::Acquire);
    #[cfg(not(lazyos_tests))]
    let bias = 0u64;
    for round in 0..ROUNDS {
        let base = 4 * round;
        if !wait_for(base + 1) {
            return false;
        }
        cpu.ipi
            .sy_t1
            .store(read_tsc_ordered().wrapping_add(bias), Ordering::Release);
        cpu.ipi.sy_state.store(base + 2, Ordering::Release);
        if !wait_for(base + 3) {
            return false;
        }
        cpu.ipi
            .sy_t2
            .store(read_tsc_ordered().wrapping_add(bias), Ordering::Release);
        cpu.ipi.sy_state.store(base + 4, Ordering::Release);
    }
    wait_for(ACCEPT)
}

/// The BSP's verdict: this AP's clock is acceptable, it may go online.
pub(super) fn accept(cpu: &percpu::Cpu) {
    cpu.ipi.sy_state.store(ACCEPT, Ordering::Release);
}

/// Forget a previous exchange (an AP being started again).
pub(super) fn reset(cpu: &percpu::Cpu) {
    cpu.ipi.sy_state.store(0, Ordering::Release);
    cpu.ipi.sy_ready.store(false, Ordering::Release);
}

/// Whether the AP has reached its half and is waiting for the BSP.
pub(super) fn ap_ready(cpu: &percpu::Cpu) -> bool {
    cpu.ipi.sy_ready.load(Ordering::Acquire)
}

/// The BSP's half: the largest TSC violation seen over the rounds, in
/// cycles (0 for coherent clocks), or `None` if the AP stopped answering
/// within `bound` cycles of `started`.
pub(super) fn bsp_side(cpu: &percpu::Cpu, started: u64, bound: u64) -> Option<u64> {
    let wait_for = |want: u64| -> bool {
        while cpu.ipi.sy_state.load(Ordering::Acquire) != want {
            if rdtsc().wrapping_sub(started) > bound {
                return false;
            }
            spin_loop();
        }
        true
    };
    let mut worst = 0;
    for round in 0..ROUNDS {
        let base = 4 * round;
        cpu.ipi.sy_state.store(base + 1, Ordering::Release);
        if !wait_for(base + 2) {
            return None;
        }
        let tb = read_tsc_ordered();
        cpu.ipi.sy_tb.store(tb, Ordering::Release);
        cpu.ipi.sy_state.store(base + 3, Ordering::Release);
        if !wait_for(base + 4) {
            return None;
        }
        let t1 = cpu.ipi.sy_t1.load(Ordering::Acquire);
        let t2 = cpu.ipi.sy_t2.load(Ordering::Acquire);
        worst = worst.max(violation(t1, tb, t2));
    }
    Some(worst)
}

/// How far `tb` lies outside `[t1, t2]`, in cycles (0 inside). Signed
/// differences, so counters that wrapped or started apart still compare.
fn violation(t1: u64, tb: u64, t2: u64) -> u64 {
    let behind = t1.wrapping_sub(tb) as i64; // > 0: the BSP read before the AP did
    let ahead = tb.wrapping_sub(t2) as i64; // > 0: the BSP read after the AP's later read
    behind.max(ahead).max(0) as u64
}
