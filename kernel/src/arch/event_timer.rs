//! The deadline timer (docs/performance-plan.md, P2.2): a one-shot local
//! APIC timer interrupt at the next task deadline, so a sleep or timeout
//! ends when it is due instead of at the next 100 Hz tick.
//!
//! The periodic tick is unchanged: the PIT still drives `TICKS`, the
//! scheduler quantum and the issue #344 catch-up. The APIC timer, unused
//! while the PIT is the tick, only adds interrupts at deadlines between two
//! ticks. Its handler runs the same expiry as a scheduler entry
//! (`task::expire_due`) and then the P1 preemption point, so a woken task
//! that should run now does.
//!
//! It exists only when the PIT is the tick and the TSC is calibrated. When
//! the APIC timer is the tick itself (a PC whose PIT is gated, or
//! `LAZYOS_TIMER=lapic`), or when the APIC cannot be brought up, deadlines
//! are honoured at ticks as before: 10 ms resolution, nothing else changes.
//! `LAZYOS_EVENT_TIMER=0` builds a kernel without it, for comparison.
//!
//! Calibration: the APIC timer is counted over a few milliseconds of TSC,
//! whose rate the PIT calibration already measured, so no second reference
//! clock is needed. Deadlines are programmed relative to `monotonic_ns`; a
//! timer that fires a little early (the two calibrations disagree slightly)
//! finds nothing due and is re-armed for the remainder.
//!
//! Only a deadline inside the current tick period is ever armed
//! (`task::expire_deadlines` decides; a later one waits for the tick that
//! begins its period). `monotonic_ns` cannot pass the end of the period the
//! last counted tick began, so a deadline beyond it could not be found due
//! until the PIT's interrupt arrives; re-arming for it meanwhile would be
//! an interrupt every `MIN_NS` for as long as that interrupt is late.
//! Arming within one period also bounds the drift between the APIC rate and
//! the clock to the calibration error over 10 ms.

use core::sync::atomic::{AtomicU64, Ordering};

use x86_64::structures::idt::InterruptStackFrame;

use super::{clock, lapic};

/// The deadline timer's vector, next to the APIC tick's.
pub const VECTOR: u8 = lapic::TIMER_VECTOR + 1;

/// Divided APIC timer counts per second; 0 when there is no deadline timer.
static RATE: AtomicU64 = AtomicU64::new(0);
/// The deadline (monotonic ns) the timer is armed for; `u64::MAX` when idle.
static ARMED: AtomicU64 = AtomicU64::new(u64::MAX);

/// Shortest interval programmed: a deadline already due, or one closer than
/// this, still gets an interrupt (the caller may be about to halt), just
/// not one that lands before the handler has returned, even under TCG.
const MIN_NS: u64 = 20_000;
/// TSC cycles the APIC timer is counted over, as a fraction of a tick.
const CALIBRATION_TICK_FRACTION: u64 = 4;

#[inline]
fn rdtsc() -> u64 {
    // SAFETY: `rdtsc` reads a CPU counter; no memory or privilege effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Whether deadlines get their own interrupt.
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn available() -> bool {
    RATE.load(Ordering::Relaxed) != 0
}

/// The APIC timer rate the deadline timer was calibrated to (0 without one).
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn rate() -> u64 {
    RATE.load(Ordering::Relaxed)
}

/// Bring up the deadline timer, with the PIT as the tick. Runs once from
/// `timer::init`, after the TSC calibration, with interrupts off. Leaves it
/// unavailable (and says why) when anything is missing.
pub fn init(madt_address: Option<u64>) {
    if option_env!("LAZYOS_EVENT_TIMER") == Some("0") {
        crate::serial_println!("timer: deadline timer off (LAZYOS_EVENT_TIMER=0)");
        return;
    }
    let per_tick = clock::cycles_per_tick();
    if per_tick == 0 {
        crate::serial_println!("timer: no deadline timer: the TSC is not calibrated");
        return;
    }
    if let Err(why) = lapic::init(madt_address) {
        crate::serial_println!("timer: no deadline timer: {why}");
        return;
    }
    let Some(rate) = measure(per_tick) else {
        crate::serial_println!("timer: no deadline timer: the APIC timer does not count");
        return;
    };
    lapic::start_oneshot(VECTOR);
    RATE.store(rate, Ordering::Relaxed);
    crate::serial_println!(
        "timer: deadline timer: APIC one-shot, {rate} counts/s ({} ns resolution)",
        1_000_000_000u64.div_ceil(rate)
    );
}

/// Count the APIC timer (divided) over a quarter tick of TSC; counts per
/// second, or `None` when it did not move.
fn measure(per_tick: u64) -> Option<u64> {
    lapic::start_free_run();
    let window = per_tick / CALIBRATION_TICK_FRACTION;
    let (apic0, tsc0) = (lapic::remaining(), rdtsc());
    while rdtsc().wrapping_sub(tsc0) < window {
        core::hint::spin_loop();
    }
    let (apic1, tsc1) = (lapic::remaining(), rdtsc());
    let counted = u128::from(apic0.checked_sub(apic1)?);
    let cycles = u128::from(tsc1.wrapping_sub(tsc0));
    let tsc_hz = u128::from(per_tick) * u128::from(super::timer_cal::HZ);
    let rate = (counted * tsc_hz / cycles.max(1)) as u64;
    (rate > 0).then_some(rate)
}

/// Divided APIC counts for `ns` at `rate` counts per second, within the
/// timer's 32-bit range (a longer wait fires early and is re-armed).
pub fn counts_for(ns: u64, rate: u64) -> u32 {
    let counts = u128::from(ns) * u128::from(rate) / 1_000_000_000;
    counts.clamp(1, u128::from(u32::MAX)) as u32
}

/// Arm the timer for `next` (monotonic ns), or stop it for `None`; `now`
/// is the current `monotonic_ns`. Re-arming for the deadline already
/// programmed does nothing, so calling this on every scheduler entry costs
/// a comparison. Call with interrupts off (the task table is held).
pub fn program(next: Option<u64>, now: u64) {
    let rate = RATE.load(Ordering::Relaxed);
    if rate == 0 {
        return;
    }
    let target = next.unwrap_or(u64::MAX);
    if ARMED.swap(target, Ordering::Relaxed) == target {
        return;
    }
    match next {
        None => lapic::set_initial_count(0),
        Some(deadline) => {
            let ns = deadline.saturating_sub(now).max(MIN_NS);
            lapic::set_initial_count(counts_for(ns, rate));
        }
    }
}

/// The deadline timer's interrupt: expire what is due and let a task it woke
/// run (P1.1). While the tick is masked nothing happens: tests and drivers
/// that mask line 0 rely on no scheduling activity at all, and the next
/// unmasked tick re-arms the timer.
///
/// Inside an interrupt window (`irq_window`) it does nothing either: the
/// interrupted syscall may hold the task table (the exit path prints, and
/// the serial drain opens windows, while holding it), and expiry takes that
/// lock, so it would spin forever with interrupts off. The deadline is then
/// honoured by the next ordinary tick, whose scheduler entry expires it and
/// re-arms the timer (now idle), as the window's PIT tick already defers.
pub extern "x86-interrupt" fn handler(_stack: InterruptStackFrame) {
    // The timer is idle now; whatever expiry decides re-arms it.
    ARMED.store(u64::MAX, Ordering::Relaxed);
    lapic::eoi();
    if super::irqchip::is_masked(0) || super::irq_window::is_open() {
        return;
    }
    crate::task::expire_due();
    crate::task::preempt_point();
}
