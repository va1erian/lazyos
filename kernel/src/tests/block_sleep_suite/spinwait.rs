//! Busy-waits for a device with interrupts off end on time and take
//! interrupts while they spin (issue #449).
//!
//! A request that may not sleep spins in `iowait::spin_until`. Its deadline
//! used to be read from `monotonic_ns`, which advances at most one tick past
//! the last tick it saw while interrupts are off, so a device that never
//! answered held the CPU until a four-billion-spin backstop, with every
//! interrupt (the timer, the i8042) shut out. The deadline is now read from
//! the TSC and the spin is a poll point (`arch::irq_window`).

use super::*;
use crate::arch::clock;
use crate::block::iowait::{self, Expect};
use crate::block::Wait;
use crate::tests::irq_window_suite::{in_syscall, kernel_task_only, BOUND_US};

/// The spin's timeout: ten ticks.
const TIMEOUT_NS: u64 = 100_000_000;
/// Ticks the spin must take through its windows (the old one took 0).
const MIN_TICKS: u64 = 3;
/// Native syscall number the fake syscall is charged to (unused by the gate).
const NR: u64 = 62;
/// Soak: waits of a few ticks each.
const SOAK_WAITS: usize = 20;
const SOAK_TIMEOUT_NS: u64 = 25_000_000;

/// TSC nanoseconds now (the suite runs calibrated).
fn now() -> Result<u64, String> {
    clock::tsc_ns().ok_or_else(|| String::from("the TSC is not calibrated"))
}

/// A device that never answers, waited for with interrupts off inside a
/// syscall: the wait gives up on its deadline, not ten ticks or minutes late,
/// and the timer's ticks arrive through windows all along.
pub fn spin_ends_by_deadline_with_windows() -> Result<(), String> {
    kernel_task_only();
    let start = now()?;
    let (answered, latency) = in_syscall(NR, || {
        iowait::wait_until(Wait::Spin, &Expect::new(), TIMEOUT_NS, || false)
    });
    let elapsed = now()? - start;
    serial_println!(
        "TEST:block_sleep_spin_ends_by_deadline:INFO:elapsed_ms={} worst_us={} ticks={} opened={}",
        elapsed / 1_000_000,
        latency.worst_us,
        latency.ticks,
        latency.opened
    );
    check!(!answered, "a device that never answered was reported done");
    check!(
        elapsed >= TIMEOUT_NS,
        "the wait gave up after {} ms, before its {} ms",
        elapsed / 1_000_000,
        TIMEOUT_NS / 1_000_000
    );
    check!(
        elapsed < TIMEOUT_NS + 50_000_000,
        "the wait took {} ms for a {} ms timeout",
        elapsed / 1_000_000,
        TIMEOUT_NS / 1_000_000
    );
    // Ten ticks fall due, but TCG on a loaded host delivers as few as six:
    // the old spin took none, and `worst_us` below bounds every stretch.
    check!(
        latency.ticks >= MIN_TICKS,
        "only {} ticks arrived in a {} ms spin",
        latency.ticks,
        TIMEOUT_NS / 1_000_000
    );
    check!(
        latency.worst_us < BOUND_US * 4,
        "interrupts stayed off {} us at a stretch inside the spin",
        latency.worst_us
    );
    Ok(())
}

/// Soak: many short never-answered waits back to back, each ending within a
/// tick of its deadline, and a wait that is answered part way through still
/// reports done.
pub fn soak_spins_stay_bounded() -> Result<(), String> {
    kernel_task_only();
    for round in 0..SOAK_WAITS {
        let start = now()?;
        let answer_at = start + SOAK_TIMEOUT_NS / 2;
        let answers = round % 2 == 1;
        let (answered, _) = in_syscall(NR, || {
            iowait::wait_until(Wait::Spin, &Expect::new(), SOAK_TIMEOUT_NS, || {
                answers && clock::tsc_ns().is_some_and(|t| t >= answer_at)
            })
        });
        let elapsed = now()? - start;
        check!(
            answered == answers,
            "round {round}: answered {answered}, expected {answers}"
        );
        let bound = if answers {
            SOAK_TIMEOUT_NS
        } else {
            SOAK_TIMEOUT_NS + 20_000_000
        };
        check!(
            elapsed < bound,
            "round {round}: {} ms (bound {} ms)",
            elapsed / 1_000_000,
            bound / 1_000_000
        );
    }
    Ok(())
}
