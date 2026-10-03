//! The wall clock (issue #368): calendar arithmetic, RTC register decoding
//! in every chip mode, `clock_settime` privilege and validation, and soaks
//! over the pure conversions and the live clock.

use super::*;
use crate::arch::rtc::{self, Raw};
use crate::ipc::credentials::{self, Cred};
use crate::wallclock;

const SYS_CLOCK_SETTIME: u64 = 227;
const SYS_CLOCK_GETTIME: u64 = 228;
const CLOCK_REALTIME: u64 = 0;
const CLOCK_MONOTONIC: u64 = 1;

const EPERM: u64 = (-1i64) as u64;
const EINVAL: u64 = (-22i64) as u64;

/// Status-B mode bits, as the chip reports them.
const BCD_24H: u8 = 0b010;
const BIN_24H: u8 = 0b110;
const BCD_12H: u8 = 0b000;

/// 2026-01-01T00:00:00Z, cross-checked against `date -u -d @1767225600`.
const Y2026: i64 = 1_767_225_600;

fn become_root() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    credentials::set(task::current(), Cred::ROOT);
    Ok(())
}

fn realtime() -> (i64, i64) {
    let mut out = [0i64; 2];
    process::linux::dispatch_for_test(
        SYS_CLOCK_GETTIME,
        CLOCK_REALTIME,
        out.as_mut_ptr() as u64,
        0,
    );
    (out[0], out[1])
}

fn settime(clock: u64, sec: i64, nsec: i64) -> u64 {
    let ts = [sec, nsec];
    process::linux::dispatch_for_test(SYS_CLOCK_SETTIME, clock, ts.as_ptr() as u64, 0)
}

fn bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

fn raw_bcd(y: u8, mo: u8, d: u8, h: u8, mi: u8, s: u8) -> Raw {
    Raw {
        second: bcd(s),
        minute: bcd(mi),
        hour: bcd(h),
        day: bcd(d),
        month: bcd(mo),
        year: bcd(y),
        century: bcd(20),
        status_b: BCD_24H,
    }
}

/// Known epoch anchors, leap-year rules, and a full day-by-day round trip.
pub fn civil_known_dates_and_leap_rules() -> Result<(), String> {
    check!(
        wallclock::days_from_civil(1970, 1, 1) == 0,
        "epoch is not day 0"
    );
    check!(
        wallclock::days_from_civil(2026, 1, 1) * 86_400 == Y2026,
        "2026-01-01 is {}",
        wallclock::days_from_civil(2026, 1, 1) * 86_400
    );
    // 2000 is a leap year (divisible by 400), 1900 and 2100 are not.
    check!(wallclock::days_in_month(2000, 2) == 29, "2000 not leap");
    check!(wallclock::days_in_month(1900, 2) == 28, "1900 leap");
    check!(wallclock::days_in_month(2100, 2) == 28, "2100 leap");
    check!(wallclock::days_in_month(2024, 2) == 29, "2024 not leap");
    // The day after Feb 28 2100 is Mar 1, not Feb 29.
    let feb28 = wallclock::days_from_civil(2100, 2, 28);
    check!(
        wallclock::civil_from_days(feb28 + 1) == (2100, 3, 1),
        "2100-02-28 + 1 day is {:?}",
        wallclock::civil_from_days(feb28 + 1)
    );
    Ok(())
}

/// Soak: every day from 1900 to 2200 converts to a civil date and back, and
/// consecutive days are consecutive.
pub fn civil_roundtrip_every_day() -> Result<(), String> {
    let start = wallclock::days_from_civil(1900, 1, 1);
    let end = wallclock::days_from_civil(2200, 12, 31);
    let mut previous = wallclock::civil_from_days(start - 1);
    for days in start..=end {
        let (y, m, d) = wallclock::civil_from_days(days);
        check!(
            wallclock::days_from_civil(y, m, d) == days,
            "day {days} round-trips to {y}-{m}-{d}"
        );
        check!(
            d >= 1 && d <= wallclock::days_in_month(y, m),
            "day {days} gave invalid {y}-{m}-{d}"
        );
        check!((y, m, d) > previous, "day {days} did not advance");
        previous = (y, m, d);
    }
    Ok(())
}

/// BCD and binary, 24-hour and 12-hour registers all decode to the same
/// instant, including the 12 AM / 12 PM edge cases.
pub fn rtc_decode_modes() -> Result<(), String> {
    let want = Y2026 + 13 * 3600 + 45 * 60 + 30; // 2026-01-01 13:45:30
    check!(
        rtc::decode(raw_bcd(26, 1, 1, 13, 45, 30)) == Some(want),
        "bcd/24h decode wrong"
    );
    let bin = Raw {
        second: 30,
        minute: 45,
        hour: 13,
        day: 1,
        month: 1,
        year: 26,
        century: 20,
        status_b: BIN_24H,
    };
    check!(rtc::decode(bin) == Some(want), "binary/24h decode wrong");
    // 12-hour: 1 PM is 0x81 with the PM flag, 12 AM is midnight, 12 PM noon.
    let mut pm = raw_bcd(26, 1, 1, 1, 45, 30);
    pm.status_b = BCD_12H;
    pm.hour |= 0x80;
    check!(rtc::decode(pm) == Some(want), "1 PM decode wrong");
    let mut midnight = raw_bcd(26, 1, 1, 12, 0, 0);
    midnight.status_b = BCD_12H;
    check!(
        rtc::decode(midnight) == Some(Y2026),
        "12 AM is not midnight"
    );
    let mut noon = raw_bcd(26, 1, 1, 12, 0, 0);
    noon.status_b = BCD_12H;
    noon.hour |= 0x80;
    check!(
        rtc::decode(noon) == Some(Y2026 + 12 * 3600),
        "12 PM is not noon"
    );
    Ok(())
}

/// Impossible register contents are rejected rather than turned into a
/// nonsense date; a garbage century register falls back to 20xx.
pub fn rtc_decode_rejects_garbage() -> Result<(), String> {
    let cases: [(&str, Raw); 7] = [
        ("month 13", raw_bcd(26, 13, 1, 0, 0, 0)),
        ("month 0", raw_bcd(26, 0, 1, 0, 0, 0)),
        ("Feb 30", raw_bcd(26, 2, 30, 0, 0, 0)),
        (
            "Feb 29 2100",
            Raw {
                century: bcd(21),
                ..raw_bcd(0, 2, 29, 0, 0, 0)
            },
        ),
        ("second 60", raw_bcd(26, 1, 1, 0, 0, 60)),
        ("hour 24", raw_bcd(26, 1, 1, 24, 0, 0)),
        (
            "bad BCD digit",
            Raw {
                minute: 0x1A,
                ..raw_bcd(26, 1, 1, 0, 0, 0)
            },
        ),
    ];
    for (name, raw) in cases {
        check!(rtc::decode(raw).is_none(), "{name} was accepted");
    }
    let odd_century = Raw {
        century: 0xFF,
        ..raw_bcd(26, 1, 1, 0, 0, 0)
    };
    check!(
        rtc::decode(odd_century) == Some(Y2026),
        "garbage century not treated as 20xx"
    );
    Ok(())
}

/// Soak: encode then decode is the identity across a multi-decade sweep in
/// every register mode.
pub fn rtc_encode_decode_roundtrip() -> Result<(), String> {
    let start = wallclock::days_from_civil(1970, 1, 1) * 86_400;
    let end = wallclock::days_from_civil(2100, 12, 31) * 86_400;
    // A prime stride walks through every hour/minute/second combination.
    let mut t = start;
    while t <= end {
        for mode in [BCD_24H, BIN_24H, BCD_12H, 0b100] {
            let back = rtc::decode(rtc::encode(t, mode));
            check!(
                back == Some(t),
                "mode {mode:#b}: {t} round-tripped to {back:?}"
            );
        }
        t += 86_399 * 7 + 13;
    }
    Ok(())
}

/// The live clock reports a plausible date (real RTC or the fallback) and
/// `CLOCK_MONOTONIC` is untouched by it.
pub fn wallclock_reads_sane_time() -> Result<(), String> {
    become_root()?;
    let (sec, nsec) = realtime();
    check!(sec >= Y2026 - 366 * 86_400, "realtime {sec} predates 2025");
    check!(
        (0..1_000_000_000).contains(&nsec),
        "nsec {nsec} out of range"
    );
    check!(
        wallclock::unix_secs() >= sec,
        "wallclock::unix_secs went backwards"
    );
    Ok(())
}

/// `clock_settime` moves realtime, rejects bad input, and needs `CAP_SYS_TIME`.
/// The original time is restored so later suites see a sane clock.
pub fn clock_settime_contract() -> Result<(), String> {
    become_root()?;
    let (orig, _) = realtime();
    let target = Y2026 + 400 * 86_400;

    check!(
        settime(CLOCK_REALTIME, target, 0) == 0,
        "root settime refused"
    );
    let (after, _) = realtime();
    check!(
        (target..target + 5).contains(&after),
        "realtime {after} after setting {target}"
    );
    check!(
        settime(CLOCK_MONOTONIC, target, 0) == EINVAL,
        "monotonic was settable"
    );
    check!(
        settime(CLOCK_REALTIME, wallclock::MAX_SET_SECS, 0) == EINVAL
            && settime(CLOCK_REALTIME, i64::MAX, 0) == EINVAL,
        "seconds past the RTC range accepted"
    );
    check!(
        settime(CLOCK_REALTIME, -1, 0) == EINVAL,
        "negative seconds accepted"
    );
    check!(
        settime(CLOCK_REALTIME, 5, 1_000_000_000) == EINVAL,
        "nsec overflow accepted"
    );
    check!(
        settime(CLOCK_REALTIME, 5, -1) == EINVAL,
        "negative nsec accepted"
    );

    credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
    check!(
        settime(CLOCK_REALTIME, orig, 0) == EPERM,
        "unprivileged settime allowed"
    );
    check!(
        settime(CLOCK_MONOTONIC, orig, 0) == EPERM,
        "capability check must precede argument checks"
    );
    credentials::set(task::current(), Cred::ROOT);
    check!(
        settime(CLOCK_REALTIME, orig, 0) == 0,
        "restoring the clock failed"
    );
    Ok(())
}

/// Soak: realtime never runs backwards over a long read loop, and repeated
/// set/get cycles land where they were told to.
pub fn soak_clock_monotone_and_settable() -> Result<(), String> {
    become_root()?;
    let (orig, _) = realtime();
    let mut last = (0i64, 0i64);
    for i in 0..100_000 {
        let now = realtime();
        check!(
            now >= last,
            "realtime went back at read {i}: {last:?} -> {now:?}"
        );
        last = now;
    }
    for i in 0..500i64 {
        let target = Y2026 + i * 7919;
        check!(
            settime(CLOCK_REALTIME, target, 0) == 0,
            "settime {i} refused"
        );
        let (got, _) = realtime();
        check!(
            (target..target + 5).contains(&got),
            "cycle {i}: {got} vs {target}"
        );
    }
    check!(settime(CLOCK_REALTIME, orig, 0) == 0, "restore failed");
    Ok(())
}

/// The monotonic clock (`arch::clock::monotonic_ns`): never decreasing,
/// always inside the current tick, finer than a tick when the TSC is
/// calibrated, and what `clock_gettime(CLOCK_MONOTONIC)` and
/// `clock_getres` report.
pub fn monotonic_clock_is_fine_and_bounded() -> Result<(), String> {
    use crate::arch::clock;
    let period = 10_000_000u64;
    let mut previous = clock::monotonic_ns();
    for round in 0..100_000u32 {
        let now = clock::monotonic_ns();
        check!(now >= previous, "round {round}: {now} < {previous}");
        let ticks = task::ticks();
        check!(
            now < (ticks + 2) * period,
            "round {round}: {now} ns is past tick {ticks}"
        );
        previous = now;
    }
    let per_tick = clock::cycles_per_tick();
    if per_tick != 0 {
        // Interrupts are off in the harness, so the tick count stands still:
        // only the TSC can move the reading. A tenth of a period must show.
        clock::resync();
        let before = clock::monotonic_ns();
        // SAFETY: `rdtsc` reads a CPU counter; no memory or privilege effects.
        let tsc = || unsafe { core::arch::x86_64::_rdtsc() };
        let start = tsc();
        while tsc().wrapping_sub(start) < per_tick / 10 {
            core::hint::spin_loop();
        }
        let after = clock::monotonic_ns();
        check!(
            after > before && after - before < period,
            "a tenth of a tick moved the clock from {before} to {after}"
        );
        check!(
            clock::resolution_ns() == 1,
            "resolution {}",
            clock::resolution_ns()
        );
    } else {
        check!(clock::resolution_ns() == period, "uncalibrated resolution");
    }
    let mut ts = [0i64; 2];
    process::linux::dispatch_for_test(
        SYS_CLOCK_GETTIME,
        CLOCK_MONOTONIC,
        ts.as_mut_ptr() as u64,
        0,
    );
    let read = ts[0] as u64 * 1_000_000_000 + ts[1] as u64;
    check!(
        (0..1_000_000_000).contains(&ts[1]) && read >= previous,
        "CLOCK_MONOTONIC read {ts:?} before {previous}"
    );
    let mut res = [0i64; 2];
    process::linux::dispatch_for_test(229, CLOCK_MONOTONIC, res.as_mut_ptr() as u64, 0);
    check!(
        res[0] == 0 && res[1] as u64 == clock::resolution_ns(),
        "clock_getres {res:?}"
    );
    let mut tv = [0i64; 2];
    process::linux::dispatch_for_test(96, tv.as_mut_ptr() as u64, 0, 0);
    check!(
        (0..1_000_000).contains(&tv[1]) && tv[0] > Y2026 - 86_400,
        "gettimeofday {tv:?}"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "wallclock_civil_known_dates",
        civil_known_dates_and_leap_rules,
    ),
    ("wallclock_soak_civil_roundtrip", civil_roundtrip_every_day),
    ("wallclock_rtc_decode_modes", rtc_decode_modes),
    (
        "wallclock_rtc_decode_rejects_garbage",
        rtc_decode_rejects_garbage,
    ),
    (
        "wallclock_soak_rtc_encode_decode",
        rtc_encode_decode_roundtrip,
    ),
    ("wallclock_reads_sane_time", wallclock_reads_sane_time),
    ("wallclock_clock_settime_contract", clock_settime_contract),
    (
        "wallclock_soak_monotone_and_settable",
        soak_clock_monotone_and_settable,
    ),
    (
        "wallclock_monotonic_clock_is_fine_and_bounded",
        monotonic_clock_is_fine_and_bounded,
    ),
];
