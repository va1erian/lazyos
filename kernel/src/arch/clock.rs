//! Tick catch-up (issue #344).
//!
//! Syscalls run with interrupts off and the 8259 holds a single pending IRQ0,
//! so a PIT period that elapses in a stretch without an interrupt window
//! (`arch::irq_window`) is lost and `TICKS` would fall behind wall time. The
//! TSC keeps counting, so each real timer entry (window ticks included) asks
//! [`periods_since_last`] how many periods passed since the previous one and
//! advances `TICKS` by that many instead of by one; [`missed_ticks`] counts
//! the periods caught up this way, which windows keep at zero.
//!
//! The TSC is calibrated once before IRQs are enabled: against PIT channel 2
//! when the PIT is the tick, or by `arch::timer` with the local APIC timer's
//! calibration (CPUID 0x15, the PM timer or the HPET) when it is not.
//! Only the *delta between two timer entries* is used, never an absolute
//! TSC-derived time, so a small calibration error cannot accumulate into drift
//! against the PIT: it can only mis-round a gap of several periods.
//!
//! The same pair (tick count, TSC at the last timer entry) gives the
//! monotonic clock its sub-tick resolution ([`monotonic_ns`]): the PIT ticks
//! plus the TSC's progress since the last entry, converted with the
//! calibration. Anchored to the ticks, it agrees with every tick-based sleep
//! deadline; the fraction only refines the reading between two ticks.

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
/// Periods caught up beyond one per entry since boot: ticks the CPU missed
/// because interrupts were off for longer than a period.
static MISSED: AtomicU64 = AtomicU64::new(0);

#[inline]
fn rdtsc() -> u64 {
    // SAFETY: `rdtsc` reads a CPU counter; no memory or privilege effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Slack (1/8 period) for TSC/PIT calibration error when flooring a gap.
const TOLERANCE_DIV: u64 = 8;

/// Periods in the gap `last..now` and the stamp to carry forward.
///
/// A single period snaps the stamp to `now`, so a small calibration error
/// cannot accumulate. A longer gap is floored, and the stamp advances by whole
/// periods only: the fraction of a period already elapsed (a late-delivered
/// IRQ0) stays credited to the next entry instead of being counted twice.
/// Never fewer than 1 period; 1 and `now` when uncalibrated.
pub fn advance(last: u64, now: u64, cycles_per_tick: u64) -> (u64, u64) {
    if cycles_per_tick == 0 {
        return (1, now);
    }
    let elapsed = now.wrapping_sub(last);
    let n = ((elapsed + cycles_per_tick / TOLERANCE_DIV) / cycles_per_tick).max(1);
    if n == 1 {
        return (1, now);
    }
    let stamp = last.wrapping_add(n * cycles_per_tick);
    // A remainder over one period is unexplained skew, not a late IRQ.
    if now.wrapping_sub(stamp) > cycles_per_tick {
        (n, now)
    } else {
        (n, stamp)
    }
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

/// Record the TSC cycles per tick measured by `arch::timer` (0 disables
/// the catch-up: every entry is one tick). Call once, before interrupts are
/// enabled, instead of [`calibrate`].
pub fn set_rate(cycles_per_tick: u64) {
    CYCLES_PER_TICK.store(cycles_per_tick, Ordering::Relaxed);
    LAST_TSC.store(rdtsc(), Ordering::Relaxed);
}

/// Timer periods since the previous call (at least 1). Call once per real
/// tick entry, with interrupts off.
pub fn periods_since_last() -> u64 {
    let now = rdtsc();
    let last = LAST_TSC.load(Ordering::Relaxed);
    let (periods, stamp) = advance(last, now, CYCLES_PER_TICK.load(Ordering::Relaxed));
    LAST_TSC.store(stamp, Ordering::Relaxed);
    if periods > 1 {
        MISSED.fetch_add(periods - 1, Ordering::Relaxed);
    }
    periods
}

/// Timer periods missed (caught up rather than taken) since boot.
pub fn missed_ticks() -> u64 {
    MISSED.load(Ordering::Relaxed)
}

/// Calibrated cycles per timer period (0 when calibration failed).
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn cycles_per_tick() -> u64 {
    CYCLES_PER_TICK.load(Ordering::Relaxed)
}

/// Nanoseconds per timer period at the 100 Hz tick.
const PERIOD_NS: u64 = 10_000_000;
/// The largest reading [`monotonic_ns`] has returned: readings never go
/// backwards, even when a timer entry snaps its TSC stamp (see [`advance`]).
static LAST_NS: AtomicU64 = AtomicU64::new(0);

/// Monotonic nanoseconds since boot: `ticks * 10 ms` plus the TSC cycles
/// elapsed since the last timer entry, converted with the calibration and
/// capped just below one period. The cap keeps the reading inside the current
/// tick, so a deadline computed from it is never more than one tick ahead of
/// the tick count sleeps wait on, even after a long stretch with the timer
/// masked (the next timer entry catches the ticks up) or a [`resync`].
/// Without a calibrated TSC it is tick-granular (10 ms).
pub fn monotonic_ns() -> u64 {
    // One consistent (ticks, stamp) pair: the timer entry updates both with
    // interrupts off, and this single CPU cannot run it in between.
    let (ticks, last) = x86_64::instructions::interrupts::without_interrupts(|| {
        (
            super::idt::TICKS.load(Ordering::Relaxed),
            LAST_TSC.load(Ordering::Relaxed),
        )
    });
    let ns = interpolate(
        ticks,
        rdtsc().wrapping_sub(last),
        CYCLES_PER_TICK.load(Ordering::Relaxed),
    );
    LAST_NS.fetch_max(ns, Ordering::Relaxed).max(ns)
}

/// `ticks` periods plus `elapsed` TSC cycles (at `per_tick` cycles a period,
/// 0 when uncalibrated) in nanoseconds, the fraction capped below one period.
pub fn interpolate(ticks: u64, elapsed: u64, per_tick: u64) -> u64 {
    let whole = ticks.saturating_mul(PERIOD_NS);
    if per_tick == 0 {
        return whole;
    }
    let elapsed = elapsed.min(per_tick - 1);
    whole + (u128::from(elapsed) * u128::from(PERIOD_NS) / u128::from(per_tick)) as u64
}

/// The resolution [`monotonic_ns`] offers: 1 ns with a calibrated TSC (the
/// reading is interpolated), else the 10 ms tick.
pub fn resolution_ns() -> u64 {
    if CYCLES_PER_TICK.load(Ordering::Relaxed) == 0 {
        PERIOD_NS
    } else {
        1
    }
}

/// Forget the gap since the last timer entry: called when the tick is unmasked,
/// so time spent with the timer deliberately off is not caught up as ticks.
pub fn resync() {
    LAST_TSC.store(rdtsc(), Ordering::Relaxed);
}

/// A raw TSC reading in nanoseconds (`None` before the TSC is calibrated),
/// for measuring a short span with interrupts off, where [`monotonic_ns`]
/// saturates one period past the last timer entry. Only the difference of two
/// readings is meaningful.
pub fn tsc_ns() -> Option<u64> {
    let per_tick = CYCLES_PER_TICK.load(Ordering::Relaxed);
    if per_tick == 0 {
        return None;
    }
    Some((u128::from(rdtsc()) * u128::from(PERIOD_NS) / u128::from(per_tick)) as u64)
}
