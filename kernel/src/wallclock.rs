//! The wall clock: UTC seconds since the Unix epoch.
//!
//! The PIT (100 Hz) only counts time since boot, so wall time is the RTC
//! sampled once plus that uptime: `now = ticks + OFFSET_CS`, in centiseconds.
//! Nothing touches the CMOS after the first read (port I/O is a VM exit, and
//! the chip only has one-second resolution anyway), and `clock_settime` just
//! moves the offset and writes the chip back so the next boot agrees.
//!
//! The kernel stays UTC-only; time zones are a userspace concern (`timed`).

use core::sync::atomic::{AtomicI64, Ordering};

use spin::Once;

use crate::arch::rtc;

/// Wall time used when the RTC is absent or reports garbage
/// (2026-01-01T00:00:00Z), so timestamps are still sane and monotonic.
pub const FALLBACK_BASE: i64 = 1_767_225_600;

/// Ticks per second of the PIT the uptime is counted in.
const HZ: i64 = 100;

/// `wall centiseconds - PIT ticks`; valid once [`init`] ran.
static OFFSET_CS: AtomicI64 = AtomicI64::new(0);
static INIT: Once<bool> = Once::new();

fn ticks() -> i64 {
    crate::task::ticks() as i64
}

/// Sample the RTC once. Idempotent; every reader calls it, because
/// filesystem stamping happens before the kernel's boot sequence reaches a
/// natural place for it. Returns whether the RTC supplied the time.
pub fn init() -> bool {
    *INIT.call_once(|| {
        let (base, from_rtc) = match rtc::read_unix() {
            Some(unix) => (unix, true),
            None => (FALLBACK_BASE, false),
        };
        OFFSET_CS.store(base * HZ - ticks(), Ordering::Relaxed);
        crate::serial_println!(
            "wallclock: {} unix={base}",
            if from_rtc {
                "rtc"
            } else {
                "fallback (rtc invalid)"
            }
        );
        from_rtc
    })
}

/// Wall time as `(seconds, centiseconds 0..100)` since the Unix epoch.
pub fn now() -> (i64, u32) {
    init();
    let cs = ticks() + OFFSET_CS.load(Ordering::Relaxed);
    (cs.div_euclid(HZ), cs.rem_euclid(HZ) as u32)
}

/// Whole seconds since the Unix epoch.
pub fn unix_secs() -> i64 {
    now().0
}

/// Convert a wall-clock instant to the PIT tick a sleeper must wait for.
/// A time already in the past saturates to tick 0, which resolves
/// immediately since ticks only advance.
pub fn wall_to_ticks(secs: u64, centis: u64) -> u64 {
    init();
    let cs = (secs as i64)
        .saturating_mul(HZ)
        .saturating_add(centis as i64);
    cs.saturating_sub(OFFSET_CS.load(Ordering::Relaxed)).max(0) as u64
}

/// Step the clock to `secs` + `centis` (0..100) and persist it to the RTC.
/// Sleepers already parked on an absolute realtime deadline keep their tick
/// deadline; only later calls see the new offset.
pub fn set(secs: i64, centis: u32) {
    init();
    OFFSET_CS.store(secs * HZ + i64::from(centis) - ticks(), Ordering::Relaxed);
    rtc::write_unix(secs);
}

/// Days from 1970-01-01 to the proleptic-Gregorian civil date (Hinnant's
/// `days_from_civil`), correct for every leap rule including the 100/400 ones.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((month + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`]: `(year, month 1..=12, day 1..=31)`.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Days in `month` of `year`.
pub fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        _ => 28,
    }
}
