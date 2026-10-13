//! `alarm`, `setitimer` and `getitimer`: remaining and interval semantics,
//! expiry, the CPU-time timers, fork and exec.

use super::*;
use crate::task::itimer::{self, Kind};
use crate::task::signal::{SIGALRM, SIGPROF, SIGVTALRM};

const ITIMER_REAL: u64 = 0;
const ITIMER_VIRTUAL: u64 = 1;
const ITIMER_PROF: u64 = 2;

/// `alarm` returns the seconds the previous alarm had left (rounded up);
/// `getitimer` agrees; `alarm(0)` cancels.
pub fn alarm_remaining() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    let ret = process::linux::dispatch_for_test(SYS_ALARM, 10, 0, 0);
    check!(ret == 0, "first alarm reported {ret} seconds left");
    let (left, interval) = getitimer(ITIMER_REAL)?;
    check!(
        left > 9 * NS && left <= 10 * NS && interval == 0,
        "after alarm(10): remaining {left} ns, interval {interval}"
    );
    let ret = process::linux::dispatch_for_test(SYS_ALARM, 5, 0, 0);
    check!(ret == 10, "alarm(5) reported {ret} seconds left, wanted 10");
    let ret = process::linux::dispatch_for_test(SYS_ALARM, 0, 0, 0);
    check!(ret == 5, "alarm(0) reported {ret} seconds left, wanted 5");
    let (left, _) = getitimer(ITIMER_REAL)?;
    check!(left == 0, "a cancelled alarm still has {left} ns");
    check!(itimer::in_use() == 0, "a cancelled alarm holds a timer");
    finish(&[p])
}

/// An alarm queues `SIGALRM` once, when due, and is then disarmed.
pub fn alarm_fires_once() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    process::linux::dispatch_for_test(SYS_ALARM, 1, 0, 0);
    let before = itimer::fired_total();
    itimer::fire_due(now() + NS / 2);
    check!(pending(p) & bit(SIGALRM) == 0, "SIGALRM before it was due");
    itimer::fire_due(now() + NS + NS / 10);
    check!(pending(p) & bit(SIGALRM) != 0, "no SIGALRM when due");
    check!(itimer::fired_total() == before + 1, "fired count off");
    itimer::fire_due(now() + 10 * NS);
    check!(itimer::fired_total() == before + 1, "a one-shot fired twice");
    let (left, _) = getitimer(ITIMER_REAL)?;
    check!(left == 0, "a fired one-shot still shows {left} ns");
    itimer::check_for_test()?;
    finish(&[p])
}

/// Bad `which`, bad `usec`, a negative value, a NULL `new`; `old` is written.
pub fn setitimer_validation() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    let ok = spec((0, 0), (3, 500_000));
    check!(setitimer(3, &ok, None) == EINVAL, "which=3 accepted");
    for bad in [
        spec((0, 1_000_000), (1, 0)),
        spec((0, 0), (1, 1_000_000)),
        spec((0, -1), (1, 0)),
        spec((-1, 0), (1, 0)),
        spec((0, 0), (-1, 0)),
    ] {
        check!(
            setitimer(ITIMER_REAL, &bad, None) == EINVAL,
            "{bad:?} accepted"
        );
    }
    check!(
        process::linux::dispatch_for_test(SYS_SETITIMER, 0, 0, 0) != 0,
        "NULL new accepted"
    );
    check!(
        setitimer(ITIMER_REAL, &ok, None) == 0,
        "valid setitimer failed"
    );
    let mut old = [-1i64; 4];
    let again = spec((2, 0), (7, 0));
    check!(
        setitimer(ITIMER_REAL, &again, Some(&mut old)) == 0,
        "second setitimer"
    );
    check!(
        old[0] == 0 && old[1] == 0 && old[2] == 3 && old[3] > 0 && old[3] <= 500_000,
        "old value {old:?}, wanted just under 3.5 s"
    );
    let (left, interval) = getitimer(ITIMER_REAL)?;
    check!(
        left > 6 * NS && left <= 7 * NS && interval == 2 * NS,
        "after second set: {left} ns left, interval {interval}"
    );
    // A zero value disarms and keeps nothing.
    let off = spec((5, 0), (0, 0));
    check!(setitimer(ITIMER_REAL, &off, None) == 0, "disarm failed");
    let (left, interval) = getitimer(ITIMER_REAL)?;
    check!(
        left == 0 && interval == 0,
        "disarmed timer reads {left}/{interval}"
    );
    finish(&[p])
}

/// A repeating timer reloads from its interval; periods that passed while
/// nothing ran are one signal plus an overrun count, never a burst.
pub fn interval_reloads() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    let t = spec((0, 10_000), (0, 10_000)); // 10 ms / 10 ms
    check!(setitimer(ITIMER_REAL, &t, None) == 0, "setitimer");
    let before = itimer::fired_total();
    let start = now();
    itimer::fire_due(start + 25_000_000);
    check!(
        itimer::fired_total() == before + 1,
        "25 ms with a 10 ms timer sent {} signals, wanted 1",
        itimer::fired_total() - before
    );
    let (left, interval) = getitimer(ITIMER_REAL)?;
    check!(
        // The fire was judged at a synthetic time 25 ms ahead, so the next
        // deadline is up to 3 periods past the real now.
        interval == 10_000_000 && left > 0 && left <= 30_000_000,
        "after a fire: {left} ns left, interval {interval}"
    );
    itimer::fire_due(start + 500_000_000);
    check!(
        itimer::fired_total() == before + 2,
        "a long gap sent a burst"
    );
    itimer::check_for_test()?;
    finish(&[p])
}

/// `ITIMER_VIRTUAL` counts only ticks that landed in ring 3; `ITIMER_PROF`
/// counts every tick charged to the process.
pub fn cpu_timers() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    let t = spec((0, 0), (0, 30_000)); // 30 ms = 3 ticks
    check!(setitimer(ITIMER_VIRTUAL, &t, None) == 0, "set virtual");
    check!(setitimer(ITIMER_PROF, &t, None) == 0, "set prof");
    task::itimer_tick_for_test(2, false); // two kernel-mode ticks
    check!(
        pending(p) & (bit(SIGVTALRM) | bit(SIGPROF)) == 0,
        "a timer fired after 2 of 3 ticks"
    );
    task::itimer_tick_for_test(1, false);
    check!(pending(p) & bit(SIGPROF) != 0, "no SIGPROF after 3 ticks");
    check!(
        pending(p) & bit(SIGVTALRM) == 0,
        "kernel ticks advanced ITIMER_VIRTUAL"
    );
    let (left, _) = getitimer(ITIMER_VIRTUAL)?;
    check!(left == 30_000_000, "virtual timer moved: {left} ns left");
    task::itimer_tick_for_test(3, true);
    check!(
        pending(p) & bit(SIGVTALRM) != 0,
        "no SIGVTALRM after 3 user ticks"
    );
    let (left, _) = getitimer(ITIMER_PROF)?;
    check!(left == 0, "a fired one-shot PROF timer reads {left}");
    itimer::check_for_test()?;
    finish(&[p])
}

/// A forked process starts with no interval timer: they are keyed by
/// address space, which the child does not share.
pub fn not_inherited() -> Result<(), String> {
    fresh()?;
    let parent = process()?;
    process::linux::dispatch_for_test(SYS_ALARM, 100, 0, 0);
    let child = task::spawn_fork().map_err(|e| format!("fork: {e}"))?;
    task::harness::set_kind(child, task::Kind::Linux);
    task::harness::switch_current(child);
    let (left, _) = getitimer(ITIMER_REAL)?;
    check!(left == 0, "the child inherited an alarm with {left} ns left");
    check!(
        process::linux::dispatch_for_test(SYS_ALARM, 0, 0, 0) == 0,
        "the child's alarm(0) reported a remainder"
    );
    task::harness::switch_current(parent);
    let (left, _) = getitimer(ITIMER_REAL)?;
    check!(left > 0, "the child's alarm(0) cleared the parent's alarm");
    finish(&[parent, child])
}

/// `exec` re-keys the interval timers to the new address space and deletes
/// POSIX timers.
pub fn exec_semantics() -> Result<(), String> {
    fresh()?;
    let (old, new) = (0x1000_u64, 0x2000_u64);
    itimer::set_interval(old, 1, Kind::Real, now(), 5 * NS, 0).map_err(|e| format!("{e:?}"))?;
    itimer::set_interval(old, 1, Kind::Prof, now(), NS, NS).map_err(|e| format!("{e:?}"))?;
    let id = itimer::create(old, 1, itimer::Notify::Process, SIGALRM, None, 1)
        .map_err(|e| format!("{e:?}"))?;
    itimer::settime(old, id, now(), Some(now() + NS), 0).map_err(|e| format!("{e:?}"))?;
    itimer::exec(old, new);
    let real = itimer::get_interval(new, Kind::Real, now());
    check!(
        real.remaining_ns > 4 * NS,
        "ITIMER_REAL lost over exec: {real:?}"
    );
    let prof = itimer::get_interval(new, Kind::Prof, now());
    check!(prof.interval_ns == NS, "ITIMER_PROF lost over exec: {prof:?}");
    check!(
        itimer::get_interval(old, Kind::Real, now()).remaining_ns == 0,
        "the old address space still has a timer"
    );
    check!(
        itimer::gettime(new, id, now()).is_err() && itimer::gettime(old, id, now()).is_err(),
        "a POSIX timer survived exec"
    );
    itimer::check_for_test()?;
    itimer::forget(new);
    check!(
        itimer::in_use() == 0,
        "forget left {} timers",
        itimer::in_use()
    );
    Ok(())
}

/// The scheduler's expiry entry (`task::expire_due`, what the deadline
/// interrupt and every tick run) fires a due timer.
pub fn scheduler_entry_fires() -> Result<(), String> {
    fresh()?;
    let p = process()?;
    let t = spec((0, 0), (0, 1)); // 1 us
    check!(setitimer(ITIMER_REAL, &t, None) == 0, "setitimer");
    let until = now() + 2_000_000;
    while now() < until {
        core::hint::spin_loop();
    }
    task::expire_due();
    check!(
        pending(p) & bit(SIGALRM) != 0,
        "expire_due did not fire the timer"
    );
    finish(&[p])
}
