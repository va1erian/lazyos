//! The tick against the CMOS RTC: over several RTC seconds, `TICKS` (and the
//! wall clock built on it) must advance 100 per second within tolerance,
//! whichever source drives it. Under QEMU the RTC follows the host clock and
//! the PIT, APIC timer and PM timer the virtual clock, which runs at host
//! speed; TCG slowness delays interrupts, and the issue #344 catch-up must
//! make up for it.

use super::*;
use crate::arch::rtc;

/// Ticks per RTC second.
const HZ: u64 = 100;
/// Allowed error, percent.
const TOLERANCE_PERCENT: u64 = 6;
/// RTC reads before giving up on an edge (each read is a few dozen port
/// accesses; this is many seconds even on fast hardware).
const MAX_READS: u32 = 5_000_000;

/// Spin until the RTC's second changes; returns the new second and the
/// tick count and wall clock seen right at the edge.
fn next_edge() -> Result<(i64, u64, i64), String> {
    let first = rtc::read_unix().ok_or("the RTC stopped answering")?;
    let start_ticks = task::ticks();
    for _ in 0..MAX_READS {
        let now = rtc::read_unix().ok_or("the RTC stopped answering")?;
        if now != first {
            let (secs, centis) = crate::wallclock::now();
            return Ok((now, task::ticks(), secs * 100 + i64::from(centis)));
        }
        if task::ticks().wrapping_sub(start_ticks) > 5 * HZ {
            return Err(format!(
                "{} ticks passed within one RTC second",
                task::ticks() - start_ticks
            ));
        }
    }
    Err("no RTC second edge".into())
}

/// Ticks and wall-clock centiseconds across `seconds` RTC seconds.
fn measure(seconds: i64) -> Result<(u64, i64), String> {
    let (start, ticks0, wall0) = next_edge()?;
    let mut last = (start, ticks0, wall0);
    while last.0 < start + seconds {
        last = next_edge()?;
    }
    check!(
        last.0 == start + seconds,
        "the RTC jumped from {start} to {}",
        last.0
    );
    Ok((last.1 - ticks0, last.2 - wall0))
}

fn within(value: u64, expected: u64) -> bool {
    value.abs_diff(expected) * 100 <= expected * TOLERANCE_PERCENT
}

fn source() -> &'static str {
    timer::info().map_or("?", |i| if i.lapic { "lapic" } else { "pit" })
}

/// Over four RTC seconds the tick advances 400 within tolerance.
pub fn tick_matches_rtc() -> Result<(), String> {
    if rtc::read_unix().is_none() {
        serial_println!("TEST:timer_tick_matches_rtc:INFO:no RTC; skipped");
        return Ok(());
    }
    kernel_only();
    const SECONDS: i64 = 4;
    let (ticks, _) = with_lines(&[0], || measure(SECONDS))?;
    let expected = SECONDS as u64 * HZ;
    serial_println!(
        "TEST:timer_tick_matches_rtc:INFO:{} ticks in {SECONDS} RTC seconds ({})",
        ticks,
        source()
    );
    check!(
        within(ticks, expected),
        "{ticks} ticks in {SECONDS} RTC seconds, expected {expected} +-{TOLERANCE_PERCENT}% ({})",
        source()
    );
    Ok(())
}

/// The wall clock (RTC at boot plus ticks) keeps pace with the RTC.
pub fn wallclock_follows_rtc() -> Result<(), String> {
    if rtc::read_unix().is_none() {
        return Ok(());
    }
    kernel_only();
    const SECONDS: i64 = 2;
    let (_, centis) = with_lines(&[0], || measure(SECONDS))?;
    let centis = u64::try_from(centis).map_err(|_| format!("the wall clock went back {centis}"))?;
    check!(
        within(centis, SECONDS as u64 * 100),
        "wall clock moved {centis} cs over {SECONDS} RTC seconds ({})",
        source()
    );
    Ok(())
}
