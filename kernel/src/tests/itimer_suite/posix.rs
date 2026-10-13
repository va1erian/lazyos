//! POSIX timers: `timer_create` and the timer syscalls, `SIGEV_SIGNAL`,
//! `SIGEV_THREAD_ID`, `SIGEV_NONE`, both clocks and absolute expiry.

use super::*;
use crate::task::itimer;
use crate::task::signal::{SIGALRM, SIGUSR1, SI_TIMER};

const SIGEV_SIGNAL: u64 = 0;
const SIGEV_NONE: u64 = 1;
const SIGEV_THREAD: u64 = 2;
const SIGEV_THREAD_ID: u64 = 4;

const CLOCK_REALTIME: u64 = 0;
const CLOCK_MONOTONIC: u64 = 1;
const CLOCK_BOOTTIME: u64 = 7;
const TIMER_ABSTIME: u64 = 1;

/// A `struct sigevent` as eight words: value, signo + notify, tid.
fn sigevent(value: u64, signo: u32, notify: u64, tid: u32) -> [u64; 8] {
    let mut event = [0u64; 8];
    event[0] = value;
    event[1] = u64::from(signo) | (notify << 32);
    event[2] = u64::from(tid);
    event
}

/// `timer_create`, returning the id or the errno.
fn create(clock: u64, event: Option<&[u64; 8]>) -> Result<i32, u64> {
    let mut id = -1i32;
    let sevp = event.map_or(0, ptr);
    let ret = process::linux::dispatch_for_test(SYS_TIMER_CREATE, clock, sevp, ptr_mut(&mut id));
    if ret == 0 {
        Ok(id)
    } else {
        Err(ret)
    }
}

/// `timer_settime` with `(interval, value)` in nanoseconds, returning the
/// errno (0) and the previous `itimerspec` as `(value, interval)`.
fn settime(id: i32, flags: u64, spec_ns: [i64; 4]) -> (u64, [i64; 4]) {
    let mut old = [-1i64; 4];
    let ret = process::linux::dispatch_args_for_test(
        SYS_TIMER_SETTIME,
        id as u64,
        flags,
        ptr(&spec_ns),
        ptr_mut(&mut old),
    );
    (ret, old)
}

/// `timer_gettime` as `(remaining_ns, interval_ns)`.
fn gettime(id: i32) -> Result<(u64, u64), u64> {
    let mut out = [0i64; 4];
    let ret = process::linux::dispatch_for_test(SYS_TIMER_GETTIME, id as u64, ptr_mut(&mut out), 0);
    if ret != 0 {
        return Err(ret);
    }
    let ns = |sec: i64, frac: i64| sec as u64 * NS + frac as u64;
    Ok((ns(out[2], out[3]), ns(out[0], out[1])))
}

/// `SIGEV_THREAD_ID` is thread-directed and carries `si_code = SI_TIMER`,
/// `si_tid`, `si_overrun` and `si_value`; a `NULL` sigevent is `SIGALRM`
/// with the id as the value.
pub fn create_and_siginfo() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    let event = sigevent(0xfeed, u32::from(SIGUSR1), SIGEV_THREAD_ID, p as u32);
    let id = create(CLOCK_MONOTONIC, Some(&event)).map_err(|e| format!("create: {e:#x}"))?;
    check!(id == 0, "the first timer id is {id}, wanted 0");
    let (ret, _) = settime(id, 0, [0, 0, 0, 5_000_000]);
    check!(ret == 0, "timer_settime returned {ret:#x}");
    itimer::fire_due(now() + NS);
    check!(
        signal::pending_thread(p) & bit(SIGUSR1) != 0,
        "SIGEV_THREAD_ID did not queue on the thread"
    );
    let info = info_of(SIGUSR1)?;
    check!(
        info.code == SI_TIMER && info.pid == id as usize && info.addr == 0xfeed && info.uid == 0,
        "siginfo {info:?}"
    );
    let default = create(CLOCK_REALTIME, None).map_err(|e| format!("create: {e:#x}"))?;
    check!(default == 1, "the second id is {default}, wanted 1");
    settime(default, 0, [0, 0, 0, 1_000_000]);
    itimer::fire_due(now() + NS);
    check!(
        pending(p) & bit(SIGALRM) != 0,
        "a NULL sigevent did not send SIGALRM"
    );
    // SIGEV_NONE counts but never signals.
    let none = create(CLOCK_MONOTONIC, Some(&sigevent(0, 0, SIGEV_NONE, 0)))
        .map_err(|e| format!("create: {e:#x}"))?;
    settime(none, 0, [0, 0, 0, 1_000_000]);
    let before = itimer::fired_total();
    itimer::fire_due(now() + NS);
    check!(
        itimer::fired_total() == before,
        "a SIGEV_NONE timer sent a signal"
    );
    itimer::check_for_test()?;
    finish(&[p])
}

/// `timer_settime` returns the old value; `timer_gettime` reports remaining
/// and interval; a periodic timer fired late counts its overrun.
pub fn settime_gettime_overrun() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    let id = create(CLOCK_MONOTONIC, None).map_err(|e| format!("create: {e:#x}"))?;
    check!(
        gettime(id) == Ok((0, 0)),
        "a new timer reads {:?}",
        gettime(id)
    );
    let (ret, old) = settime(id, 0, [0, 1_000_000, 1, 0]); // 1 s, then every 1 ms
    check!(
        ret == 0 && old == [0, 0, 0, 0],
        "first settime: {ret:#x} {old:?}"
    );
    let (left, interval) = gettime(id).map_err(|e| format!("gettime {e:#x}"))?;
    check!(
        left > 900_000_000 && left <= NS && interval == 1_000_000,
        "gettime: {left} ns left, interval {interval}"
    );
    let (ret, old) = settime(id, 0, [0, 0, 0, 0]);
    check!(
        ret == 0
            && old[1] == 1_000_000
            && (old[2] as u64 * NS + old[3] as u64) > 900_000_000
            && (old[2] as u64 * NS + old[3] as u64) <= NS,
        "disarm returned {old:?}"
    );
    check!(gettime(id) == Ok((0, 0)), "a disarmed timer is not zero");
    settime(id, 0, [0, 1_000_000, 0, 1_000_000]);
    itimer::fire_due(now() + 50_000_000);
    let overrun = process::linux::dispatch_for_test(SYS_TIMER_GETOVERRUN, id as u64, 0, 0);
    check!(
        (40..=60).contains(&overrun),
        "overrun after 50 ms of a 1 ms timer: {overrun}"
    );
    check!(pending(p) & bit(SIGALRM) != 0, "no signal for a late timer");
    finish(&[p])
}

/// Absolute expiry on each clock, the past firing at once, `CLOCK_BOOTTIME`
/// as an alias, and the CPU-time clocks refused.
pub fn absolute_and_clocks() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    let mono = create(CLOCK_MONOTONIC, None).map_err(|e| format!("{e:#x}"))?;
    let at = now() + 50_000_000;
    let (ret, _) = settime(
        mono,
        TIMER_ABSTIME,
        [0, 0, (at / NS) as i64, (at % NS) as i64],
    );
    check!(ret == 0, "absolute monotonic settime {ret:#x}");
    let (left, _) = gettime(mono).map_err(|e| format!("{e:#x}"))?;
    check!(left > 0 && left <= 50_000_000, "absolute: {left} ns left");

    let real = create(CLOCK_REALTIME, None).map_err(|e| format!("{e:#x}"))?;
    let (sec, nanos) = crate::wallclock::now_ns();
    let at_ns = nanos as i64 + 80_000_000;
    let (ret, _) = settime(
        real,
        TIMER_ABSTIME,
        [0, 0, sec + at_ns / NS as i64, at_ns % NS as i64],
    );
    check!(ret == 0, "absolute realtime settime {ret:#x}");
    let (left, _) = gettime(real).map_err(|e| format!("{e:#x}"))?;
    check!(
        left > 0 && left <= 80_000_000,
        "absolute realtime: {left} ns left"
    );

    // An absolute time already past fires at the next expiry pass.
    let past = create(CLOCK_REALTIME, Some(&sigevent(7, 12, SIGEV_SIGNAL, 0)))
        .map_err(|e| format!("{e:#x}"))?;
    settime(past, TIMER_ABSTIME, [0, 0, 1, 0]);
    let before = itimer::fired_total();
    itimer::fire_due(now());
    check!(
        itimer::fired_total() == before + 1,
        "a past absolute time did not fire at once"
    );
    check!(pending(p) & bit(12) != 0, "SIGUSR2 (12) not pending");

    check!(
        create(CLOCK_BOOTTIME, None).is_ok(),
        "CLOCK_BOOTTIME refused"
    );
    for clock in [2, 3, 4, 99] {
        check!(
            create(clock, None) == Err(EINVAL),
            "timer clock {clock} accepted"
        );
    }
    finish(&[p])
}

/// Flags, signal numbers, sigevent kinds, thread ids and bad timer ids.
pub fn validation() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    let sig = |signo, notify, tid| Some(sigevent(0, signo, notify, tid));
    for (what, event) in [
        ("SIGKILL", sig(9, SIGEV_SIGNAL, 0)),
        ("SIGSTOP", sig(19, SIGEV_SIGNAL, 0)),
        ("signal 0", sig(0, SIGEV_SIGNAL, 0)),
        ("signal 65", sig(65, SIGEV_SIGNAL, 0)),
        ("SIGEV_THREAD", sig(10, SIGEV_THREAD, 0)),
        (
            "a thread of another process",
            sig(10, SIGEV_THREAD_ID, 4000),
        ),
        ("tid 0", sig(10, SIGEV_THREAD_ID, 0)),
    ] {
        check!(
            create(CLOCK_MONOTONIC, event.as_ref()) == Err(EINVAL),
            "{what} accepted"
        );
    }
    check!(
        itimer::in_use() == 0,
        "a refused create left {} timers",
        itimer::in_use()
    );
    let id = create(CLOCK_MONOTONIC, None).map_err(|e| format!("{e:#x}"))?;
    check!(settime(id, 2, [0, 0, 1, 0]).0 == EINVAL, "flag 2 accepted");
    check!(
        settime(id, 0, [0, 0, 0, 1_000_000_000]).0 == EINVAL,
        "nsec 1e9 accepted"
    );
    check!(
        settime(id, 0, [0, -1, 1, 0]).0 == EINVAL,
        "negative interval accepted"
    );
    check!(settime(77, 0, [0, 0, 1, 0]).0 == EINVAL, "unknown id");
    check!(gettime(77) == Err(EINVAL), "gettime of an unknown id");
    check!(
        process::linux::dispatch_for_test(SYS_TIMER_GETOVERRUN, 77, 0, 0) == EINVAL,
        "getoverrun of an unknown id"
    );
    check!(
        process::linux::dispatch_for_test(SYS_TIMER_DELETE, 77, 0, 0) == EINVAL,
        "delete of an unknown id"
    );
    finish(&[p])
}

/// A process holds a bounded number of POSIX timers (`EAGAIN`), ids are
/// reused lowest first, and a deleted timer never fires.
pub fn limit_and_delete() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    let mut ids = alloc::vec::Vec::new();
    for _ in 0..itimer::MAX_POSIX_PER_PROCESS {
        ids.push(create(CLOCK_MONOTONIC, None).map_err(|e| format!("create: {e:#x}"))?);
    }
    check!(
        create(CLOCK_MONOTONIC, None) == Err(EAGAIN),
        "the per-process limit was not enforced"
    );
    let victim = ids[5];
    settime(victim, 0, [0, 0, 0, 1_000_000]);
    check!(
        process::linux::dispatch_for_test(SYS_TIMER_DELETE, victim as u64, 0, 0) == 0,
        "timer_delete failed"
    );
    let before = itimer::fired_total();
    itimer::fire_due(now() + NS);
    check!(itimer::fired_total() == before, "a deleted timer fired");
    let reused = create(CLOCK_MONOTONIC, None).map_err(|e| format!("{e:#x}"))?;
    check!(reused == victim, "id {reused} not reused, wanted {victim}");
    for id in ids.iter().filter(|&&id| id != victim) {
        process::linux::dispatch_for_test(SYS_TIMER_DELETE, *id as u64, 0, 0);
    }
    process::linux::dispatch_for_test(SYS_TIMER_DELETE, reused as u64, 0, 0);
    check!(itimer::in_use() == 0, "{} timers leaked", itimer::in_use());
    itimer::check_for_test()?;
    finish(&[p])
}
