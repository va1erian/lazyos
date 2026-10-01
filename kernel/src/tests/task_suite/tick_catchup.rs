//! Tick catch-up (issue #344): periods that elapse with interrupts off are not
//! lost, because each real timer entry advances `TICKS` by the periods the
//! TSC saw since the previous one.

use super::*;
use crate::arch::{clock, pic};

/// The rounding rule: nearest period, at least one, one when uncalibrated.
pub fn clock_periods_rounding() -> Result<(), String> {
    const P: u64 = 1_000;
    check!(clock::periods_in(0, P) == 1, "an empty gap is not one period");
    check!(clock::periods_in(P - 1, P) == 1, "just under a period");
    check!(clock::periods_in(P + P / 4, P) == 1, "a period and a quarter");
    check!(clock::periods_in(5 * P + P / 3, P) == 5, "five and a third");
    check!(clock::periods_in(5 * P + 2 * P / 3, P) == 6, "five and two thirds");
    check!(clock::periods_in(u64::MAX / 2, 0) == 1, "uncalibrated must be 1");
    Ok(())
}

/// A lone runnable kernel task, so a real tick resumes the test.
fn kernel_only() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
}

/// Run `body` with IRQ0 alone unmasked and interrupts on, restoring the
/// harness state (everything masked, `IF=0`) afterwards.
fn with_timer_on<T>(body: impl FnOnce() -> T) -> T {
    let saved: [bool; 16] = core::array::from_fn(|l| pic::is_masked(l as u8));
    for line in 0..16u8 {
        pic::set_masked(line, line != 0);
    }
    x86_64::instructions::interrupts::enable();
    let result = body();
    x86_64::instructions::interrupts::disable();
    for (line, masked) in saved.into_iter().enumerate() {
        pic::set_masked(line as u8, masked);
    }
    result
}

/// Spin until `TICKS` moves from `from`; false if the PIT never fires.
fn wait_for_tick(from: u64) -> bool {
    for _ in 0..50_000_000u32 {
        if task::ticks() != from {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// With the timer running, spin `periods` periods with interrupts off, then
/// let the pending IRQ0 fire; returns how far `TICKS` moved (counted from a
/// fresh tick, so the idle time before the test does not count).
fn lost_then_caught_up(periods: u64) -> Option<u64> {
    let per_tick = clock::cycles_per_tick();
    with_timer_on(|| {
        // Re-sync the clock's last-entry stamp to a real tick first.
        let sync = task::ticks();
        if !wait_for_tick(sync) {
            return None;
        }
        let before = task::ticks();
        x86_64::instructions::interrupts::without_interrupts(|| {
            // SAFETY: `rdtsc` only reads the time-stamp counter.
            let start = unsafe { core::arch::x86_64::_rdtsc() };
            // SAFETY: as above.
            while unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start)
                < periods * per_tick
            {
                core::hint::spin_loop();
            }
        });
        wait_for_tick(before).then(|| task::ticks() - before)
    })
}

/// A long interrupts-off spin loses no ticks (one period of slack).
pub fn interrupts_off_spin_keeps_clock() -> Result<(), String> {
    check!(clock::cycles_per_tick() != 0, "the TSC was not calibrated");
    kernel_only();
    let moved = lost_then_caught_up(8).ok_or("no timer tick after the spin")?;
    check!(
        (7..=10).contains(&moved),
        "an 8-period interrupts-off spin moved TICKS by {moved}"
    );
    Ok(())
}

/// Soak: many gaps of varied length stay within one period each, and the
/// total never runs ahead of the spun time.
pub fn interrupts_off_spin_soak() -> Result<(), String> {
    kernel_only();
    let (mut spun, mut moved) = (0u64, 0u64);
    for round in 0..40u64 {
        let periods = 2 + round % 7;
        let m = lost_then_caught_up(periods).ok_or("no timer tick after a spin")?;
        check!(
            m + 1 >= periods && m <= periods + 2,
            "a {periods}-period spin moved TICKS by {m}"
        );
        spun += periods;
        moved += m;
    }
    check!(
        moved + 40 >= spun && moved <= spun + 80,
        "40 spins of {spun} periods moved TICKS by {moved}"
    );
    Ok(())
}
