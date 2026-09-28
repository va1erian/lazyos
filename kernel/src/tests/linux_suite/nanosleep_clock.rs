//! `nanosleep` and `clock_nanosleep`: relative sleeps, absolute
//! deadlines in the past and future, bad clocks/flags, and a soak.

use super::*;

/// A relative `nanosleep` blocks for roughly the requested duration.
pub fn nanosleep_relative_duration() -> Result<(), String> {
    fresh()?;
    let before = task::ticks();
    // 30ms => 3 ticks at 100 Hz.
    let req = [0i64, 30_000_000];
    let mut rem = [0i64; 2];
    let ret = nanosleep(&req, &mut rem);
    check!(ret == 0, "relative nanosleep returned {ret:#x}");
    let elapsed = task::ticks() - before;
    check!(
        (3..=30).contains(&elapsed),
        "relative nanosleep took an unexpected number of ticks: {elapsed}"
    );
    Ok(())
}

/// `clock_nanosleep` with `TIMER_ABSTIME` and a deadline already in the
/// past returns immediately, on both clocks `clock_gettime` reports.
pub fn clock_nanosleep_absolute_past_returns_immediately() -> Result<(), String> {
    fresh()?;
    for &clock in &[CLOCK_REALTIME, CLOCK_MONOTONIC] {
        let before = task::ticks();
        // (0, 0) is long before both the boot epoch and the fixed
        // realtime base, so it is in the past on either clock.
        let req = [0i64, 0i64];
        let mut rem = [0i64; 2];
        let ret = clock_nanosleep(clock, TIMER_ABSTIME, &req, &mut rem);
        check!(
            ret == 0,
            "past absolute deadline on clock {clock} returned {ret:#x}"
        );
        let elapsed = task::ticks() - before;
        check!(
            elapsed <= 2,
            "past absolute deadline on clock {clock} blocked for {elapsed} ticks"
        );
    }
    Ok(())
}

/// A `TIMER_ABSTIME` deadline slightly in the future waits until that
/// instant, not for the deadline's raw value read as a duration.
///
/// Deliberately uses `CLOCK_REALTIME`, not `CLOCK_MONOTONIC`: this early
/// in boot, monotonic "now" is itself only a few ticks past zero, so a
/// short relative delta and a monotonic absolute deadline are almost the
/// same bit pattern and the old bug (reading the deadline as a duration)
/// would go unnoticed. `CLOCK_REALTIME`'s fixed epoch base
/// (`REALTIME_BASE`, ~56 years) makes the two unmistakably different: the
/// old code, given a `CLOCK_REALTIME` deadline, would try to sleep for
/// about that many seconds (matching the issue's "roughly the whole
/// uptime" on `CLOCK_MONOTONIC`, just far more dramatic on this clock),
/// so a regression here fails by timeout, not by a fast assertion.
pub fn clock_nanosleep_absolute_future_waits_until_deadline() -> Result<(), String> {
    fresh()?;
    let (sec, nsec) = clock_now(CLOCK_REALTIME);
    let (dsec, dnsec) = add_nanos(sec, nsec, 40_000_000); // 40ms ahead
    let req = [dsec, dnsec];
    let mut rem = [0i64; 2];
    let before = task::ticks();
    let ret = clock_nanosleep(CLOCK_REALTIME, TIMER_ABSTIME, &req, &mut rem);
    check!(ret == 0, "future absolute deadline returned {ret:#x}");
    let elapsed = task::ticks() - before;
    check!(
        elapsed >= 3,
        "future absolute deadline returned too early: elapsed={elapsed} ticks"
    );
    check!(
        elapsed <= 30,
        "future absolute deadline waited far longer than requested \
         (treated as a duration instead of a deadline?): elapsed={elapsed} ticks, \
         uptime-before={before} ticks"
    );
    Ok(())
}

/// Unknown clocks and unknown flag bits are rejected with `-EINVAL`,
/// including when `TIMER_ABSTIME` is combined with an unknown bit; the
/// timespec validation (negative seconds/nanoseconds, and nanoseconds
/// outside `0..1_000_000_000`) still applies.
pub fn clock_nanosleep_bad_clock_and_flags() -> Result<(), String> {
    fresh()?;
    let req = [0i64, 0i64];
    let mut rem = [0i64; 2];

    let ret = clock_nanosleep(2, 0, &req, &mut rem);
    check!(ret == EINVAL, "unknown clock accepted: {ret:#x}");

    let ret = clock_nanosleep(CLOCK_MONOTONIC, 2, &req, &mut rem);
    check!(ret == EINVAL, "unknown flag bit accepted: {ret:#x}");

    let ret = clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME | 2, &req, &mut rem);
    check!(
        ret == EINVAL,
        "TIMER_ABSTIME combined with an unknown bit accepted: {ret:#x}"
    );

    let bad_req = [-1i64, 0i64];
    let ret = clock_nanosleep(CLOCK_MONOTONIC, 0, &bad_req, &mut rem);
    check!(ret == EINVAL, "negative seconds accepted: {ret:#x}");

    let bad_req = [0i64, -1i64];
    let ret = clock_nanosleep(CLOCK_MONOTONIC, 0, &bad_req, &mut rem);
    check!(ret == EINVAL, "negative nanoseconds accepted: {ret:#x}");

    let bad_req = [0i64, 1_000_000_000i64];
    let ret = clock_nanosleep(CLOCK_MONOTONIC, 0, &bad_req, &mut rem);
    check!(ret == EINVAL, "nanoseconds >= 1s accepted: {ret:#x}");
    Ok(())
}

/// Soak: many short `TIMER_ABSTIME` sleeps in a row, each one tick ahead
/// of the clock read just before it, catch leaks/races in the deadline
/// conversion and the wait-queue path under repeated use.
pub fn clock_nanosleep_soak_absolute() -> Result<(), String> {
    fresh()?;
    const ITERATIONS: usize = 40;
    let mut rem = [0i64; 2];
    let start = task::ticks();
    for i in 0..ITERATIONS {
        let (sec, nsec) = clock_now(CLOCK_MONOTONIC);
        let (dsec, dnsec) = add_nanos(sec, nsec, 10_000_000); // 10ms = 1 tick ahead
        let req = [dsec, dnsec];
        let ret = clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &req, &mut rem);
        check!(ret == 0, "soak iteration {i} returned {ret:#x}");
    }
    let elapsed = task::ticks() - start;
    check!(
        elapsed >= ITERATIONS as u64,
        "soak sleeps finished faster than requested: elapsed={elapsed} ticks for {ITERATIONS} iterations"
    );
    check!(
        elapsed <= (ITERATIONS as u64) * 5,
        "soak sleeps took far longer than requested: elapsed={elapsed} ticks"
    );
    serial_println!(
        "TEST:linux_clock_nanosleep_soak_absolute:INFO:iterations={ITERATIONS} elapsed_ticks={elapsed}"
    );
    Ok(())
}
