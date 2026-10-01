//! Tick catch-up (issue #344).
//!
//! Syscalls run with interrupts off and the 8259 holds a single pending IRQ0,
//! so every PIT period that elapses inside a long syscall is lost and `TICKS`
//! falls behind wall time. The TSC keeps counting, so each real timer entry
//! asks [`periods_since_last`] how many periods passed since the previous one
//! and advances `TICKS` by that many instead of by one.
//!
//! The TSC is calibrated once against PIT channel 2 before IRQs are enabled.
//! Only the *delta between two timer entries* is used, never an absolute
//! TSC-derived time, so a small calibration error cannot accumulate into drift
//! against the PIT: it can only mis-round a gap of several periods.

use super::io::{inb, outb};
use core::sync::atomic::{AtomicU64, Ordering};

/// PIT input clock, Hz.
const PIT_HZ: u64 = 1_193_182;
/// Channel 2 count timed during calibration (about 10 ms).
const CALIBRATION_COUNT: u16 = 11_932;
/// Spin bound so a PIT that never raises OUT2 cannot hang boot.
const CALIBRATION_SPINS: u32 = 50_000_000;

/// TSC cycles per timer period; 0 until calibrated (then every entry is 1 tick).
static CYCLES_PER_TICK: AtomicU64 = AtomicU64::new(0);
/// TSC at the previous timer entry.
static LAST_TSC: AtomicU64 = AtomicU64::new(0);

#[inline]
fn rdtsc() -> u64 {
    // SAFETY: `rdtsc` reads a CPU counter; no memory or privilege effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Periods represented by `elapsed` TSC cycles, rounded to nearest and never
/// below 1 (an entry is by definition at least one period). 1 when
/// uncalibrated.
pub fn periods_in(elapsed: u64, cycles_per_tick: u64) -> u64 {
    if cycles_per_tick == 0 {
        return 1;
    }
    ((elapsed + cycles_per_tick / 2) / cycles_per_tick).max(1)
}

/// Time PIT channel 2 for ~10 ms against the TSC and record the cycles per
/// timer period of `tick_hz`.
///
/// # Safety
/// Call once from `idt::init_hardware`, before interrupts are enabled. Takes
/// ownership of PIT channel 2 and the speaker gate (port 0x61) for the
/// duration, and leaves the speaker off.
pub unsafe fn calibrate(tick_hz: u32) {
    let gate = inb(0x61);
    outb(0x61, gate & !0x03); // gate low, speaker off
    outb(0x43, 0xB0); // channel 2, lobyte/hibyte, mode 0 (interrupt on terminal count)
    outb(0x42, (CALIBRATION_COUNT & 0xFF) as u8);
    outb(0x42, (CALIBRATION_COUNT >> 8) as u8);
    let start = rdtsc();
    outb(0x61, (gate & !0x02) | 0x01); // raise the gate: counting starts
    let mut spins = 0;
    while inb(0x61) & 0x20 == 0 {
        spins += 1;
        if spins >= CALIBRATION_SPINS {
            outb(0x61, gate);
            return;
        }
    }
    let cycles = rdtsc().wrapping_sub(start);
    outb(0x61, gate);
    let per_tick = cycles as u128 * (PIT_HZ / tick_hz as u64) as u128 / CALIBRATION_COUNT as u128;
    CYCLES_PER_TICK.store(per_tick as u64, Ordering::Relaxed);
    LAST_TSC.store(rdtsc(), Ordering::Relaxed);
}

/// Timer periods since the previous call (at least 1). Call once per real
/// PIT entry, with interrupts off.
pub fn periods_since_last() -> u64 {
    let now = rdtsc();
    let last = LAST_TSC.swap(now, Ordering::Relaxed);
    periods_in(
        now.wrapping_sub(last),
        CYCLES_PER_TICK.load(Ordering::Relaxed),
    )
}

/// Calibrated cycles per timer period (0 when calibration failed).
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn cycles_per_tick() -> u64 {
    CYCLES_PER_TICK.load(Ordering::Relaxed)
}

/// Forget the gap since the last timer entry: called when IRQ0 is unmasked,
/// so time spent with the timer deliberately off is not caught up as ticks.
pub fn resync() {
    LAST_TSC.store(rdtsc(), Ordering::Relaxed);
}
