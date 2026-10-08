//! `t_time` — `Instant`/`SystemTime` (`clock_gettime`) and sleeping: std's
//! relative `thread::sleep`, and musl's `clock_nanosleep` both relative and
//! with `TIMER_ABSTIME` on the monotonic and realtime clocks (issue #669:
//! an absolute deadline once ran as a duration, so apps avoided std's sleep).

mod common;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Timespec {
    sec: i64,
    nsec: i64,
}

unsafe extern "C" {
    fn clock_gettime(clock: i32, ts: *mut Timespec) -> i32;
    fn clock_nanosleep(clock: i32, flags: i32, req: *const Timespec, rem: *mut Timespec) -> i32;
}

const CLOCK_REALTIME: i32 = 0;
const CLOCK_MONOTONIC: i32 = 1;
const TIMER_ABSTIME: i32 = 1;

/// Every sleep asks for this long.
const NAP: Duration = Duration::from_millis(50);
/// Longer than any honest sleep of [`NAP`] takes, even under TCG; far
/// shorter than an absolute deadline misread as a duration (seconds since
/// boot, or decades on the realtime clock).
const TOO_LONG: Duration = Duration::from_secs(2);

fn now(clock: i32) -> Timespec {
    let mut ts = Timespec::default();
    // SAFETY: `ts` is a writable `struct timespec`.
    unsafe { clock_gettime(clock, &mut ts) };
    ts
}

fn plus(ts: Timespec, by: Duration) -> Timespec {
    let total = ts.nsec + by.subsec_nanos() as i64;
    Timespec {
        sec: ts.sec + by.as_secs() as i64 + total / 1_000_000_000,
        nsec: total % 1_000_000_000,
    }
}

/// Time `sleep`, failing the fixture unless it lasted [`NAP`] (and not
/// [`TOO_LONG`]).
fn timed(what: &str, sleep: impl FnOnce() -> i32) {
    let start = Instant::now();
    let code = sleep();
    let took = start.elapsed();
    if code != 0 {
        common::fail("time", &format!("{what} returned {code}"));
    }
    // The realtime clock may be read more coarsely than the monotonic one
    // `Instant` uses, so a deadline on it can land a little early.
    if took < NAP - Duration::from_millis(15) || took > TOO_LONG {
        common::fail("time", &format!("{what} slept {took:?} for {NAP:?}"));
    }
    println!("ABI:time:SLEEP:{what}:{}us", took.as_micros());
}

fn nanosleep(clock: i32, flags: i32, req: Timespec) -> i32 {
    let mut rem = Timespec::default();
    // SAFETY: `req` and `rem` are live `struct timespec`s for the call.
    unsafe { clock_nanosleep(clock, flags, &req, &mut rem) }
}

fn main() {
    let start = Instant::now();
    std::thread::sleep(Duration::from_millis(1));
    let elapsed = start.elapsed();
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    if elapsed < Duration::from_micros(100) || since_epoch.as_secs() == 0 {
        common::fail("time", "clock did not advance");
    }

    timed("thread_sleep", || {
        std::thread::sleep(NAP);
        0
    });
    let relative = Timespec {
        sec: 0,
        nsec: NAP.as_nanos() as i64,
    };
    timed("relative_monotonic", || {
        nanosleep(CLOCK_MONOTONIC, 0, relative)
    });
    for (name, clock) in [
        ("abstime_monotonic", CLOCK_MONOTONIC),
        ("abstime_realtime", CLOCK_REALTIME),
    ] {
        timed(name, || {
            nanosleep(clock, TIMER_ABSTIME, plus(now(clock), NAP))
        });
    }
    // A deadline already behind the clock returns at once.
    let start = Instant::now();
    let code = nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, Timespec::default());
    if code != 0 || start.elapsed() > Duration::from_millis(30) {
        common::fail("time", "a past TIMER_ABSTIME deadline blocked");
    }
    common::pass("time");
}
