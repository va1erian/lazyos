//! The per-line rate limit (issue #496, `dev::throttle`).
//!
//! A level-triggered line whose device keeps asserting re-fires the moment
//! its round ends. Here the "device" is the test firing the line again each
//! time the controller would deliver it (only while it is unmasked): past
//! `ROUNDS_PER_TICK` rounds in one tick the line must stay masked until the
//! bottom half runs on a later tick, and a line under the limit is never
//! held.

use super::fixture::*;
use super::irq::{rig, Rig};
use super::*;
use crate::dev::intx;
use crate::dev::throttle::{self, ROUNDS_PER_TICK};

pub(super) const CASES: &[(&str, Test)] = &[
    ("dev_irq_rate_holds_a_storm", holds_a_storm),
    ("dev_irq_rate_spares_a_busy_line", spares_a_busy_line),
    ("dev_irq_rate_soak_storms", soak_storms),
];

/// Whether `line` can be held (level-triggered, or on the 8259); a skip is
/// reported otherwise.
fn level(line: u8, test: &str) -> bool {
    let level = crate::arch::irqchip::masked_keeps_request(line);
    if !level {
        serial_println!("TEST:{test}:INFO:line {line} is edge-triggered here; skipped");
    }
    level
}

/// Deliver `line` at tick `now` for as long as the controller would let it
/// (unmasked), up to `limit` rounds, with the driver serving each one at
/// once. Returns the rounds delivered.
fn storm(r: &Rig, line: u8, now: u64, limit: u32) -> Result<u32, String> {
    let mut rounds = 0;
    while !masked(line) && rounds < limit {
        fire(line, now);
        enter(r.slot)?;
        take_irq(r.endpoint)?;
        expect_ok(r.ack(), "ack")?;
        rounds += 1;
    }
    Ok(rounds)
}

/// A line that re-asserts on every unmask is held after `ROUNDS_PER_TICK`
/// rounds in one tick, stays held for the rest of that tick, and is let go
/// by the next tick's bottom half.
pub fn holds_a_storm() -> Result<(), String> {
    if !level(LINE_A, "dev_irq_rate_holds_a_storm") {
        return Ok(());
    }
    let fx = Fixture::new()?;
    let r = rig(LINE_A, false, true)?;
    let holds = throttle::holds();
    let now = 100;
    let rounds = storm(&r, LINE_A, now, 10 * ROUNDS_PER_TICK)?;
    check!(
        rounds == ROUNDS_PER_TICK + 1,
        "{rounds} rounds in one tick (limit {ROUNDS_PER_TICK})"
    );
    check!(masked(LINE_A), "the storming line was not held");
    check!(throttle::holds() == holds + 1, "the hold was not counted");
    check!(
        throttle::any_held(),
        "the hold is not visible to the bottom half"
    );
    intx::service_at(now);
    check!(masked(LINE_A), "the line was let go in the same tick");
    intx::service_at(now + 1);
    check!(!masked(LINE_A), "the next tick did not let the line go");
    check!(!throttle::any_held(), "the hold outlived its release");
    check!(
        r.flags()? == (true, false, false),
        "the claim after a hold: {:?}",
        r.flags()?
    );
    leave(&fx);
    Ok(())
}

/// A busy but serviced line (a NIC under load: half the limit every tick,
/// for many ticks) is never held.
pub fn spares_a_busy_line() -> Result<(), String> {
    let fx = Fixture::new()?;
    let r = rig(LINE_A, false, true)?;
    let holds = throttle::holds();
    for tick in 0..50u64 {
        let rounds = storm(&r, LINE_A, 1_000 + tick, ROUNDS_PER_TICK / 2)?;
        check!(
            rounds == ROUNDS_PER_TICK / 2,
            "tick {tick}: only {rounds} rounds"
        );
    }
    check!(!masked(LINE_A), "a serviced line was left masked");
    check!(
        throttle::holds() == holds,
        "a line under the limit was held"
    );
    leave(&fx);
    Ok(())
}

/// Soak: 200 ticks alternating storms and quiet ticks on two lines, one with
/// a shared pair of claimants. Every tick delivers at most the limit plus the
/// round that crossed it per line, every held line recovers on the next
/// tick, and nothing is left held or masked at the end.
pub fn soak_storms() -> Result<(), String> {
    if !level(LINE_A, "dev_irq_rate_soak_storms") || !level(LINE_B, "dev_irq_rate_soak_storms") {
        return Ok(());
    }
    let fx = Fixture::new()?;
    let a = rig(LINE_A, false, true)?;
    let b = rig(LINE_B, true, true)?;
    let c = rig(LINE_B, true, true)?;
    let mut held_ticks = 0;
    for tick in 0..200u64 {
        let now = 5_000 + tick;
        intx::service_at(now);
        check!(
            !masked(LINE_A) && !masked(LINE_B),
            "tick {tick}: a line held on the previous tick was not let go"
        );
        let limit = if tick % 3 == 0 {
            4 * ROUNDS_PER_TICK
        } else {
            3
        };
        let rounds = storm(&a, LINE_A, now, limit)?;
        check!(
            rounds <= ROUNDS_PER_TICK + 1,
            "tick {tick}: {rounds} rounds on line A"
        );
        // The shared line: both claimants serve each round before it unmasks.
        let mut shared = 0;
        while !masked(LINE_B) && shared < limit {
            fire(LINE_B, now);
            for r in [&b, &c] {
                enter(r.slot)?;
                take_irq(r.endpoint)?;
                expect_ok(r.ack(), "shared ack")?;
            }
            shared += 1;
        }
        check!(
            shared <= ROUNDS_PER_TICK + 1,
            "tick {tick}: {shared} rounds on line B"
        );
        if masked(LINE_A) {
            held_ticks += 1;
        }
    }
    intx::service_at(10_000);
    check!(held_ticks >= 60, "only {held_ticks} storm ticks were held");
    check!(!throttle::any_held(), "a line is still held after the soak");
    check!(
        !masked(LINE_A) && !masked(LINE_B),
        "a line was left masked after the soak"
    );
    leave(&fx);
    Ok(())
}
