//! Clocks and sleeping: `gettimeofday`, `clock_gettime`/`clock_getres`,
//! `nanosleep`/`clock_nanosleep`, and `getrandom`. [`millis_to_ticks`] is
//! shared with the poll-style waits in [`super::io`] and [`super::epoll`],
//! since a millisecond timeout is the common currency across all of them.

use crate::task::{self, WakeReason};
use crate::user_ptr;

use crate::ipc::credentials::{self, CAP_SYS_TIME};

use super::errno::{err, EFAULT, EINTR, EINVAL, EPERM};
use super::uaccess::fill_random;

const CLOCK_REALTIME: u64 = 0;
pub(super) const CLOCK_MONOTONIC: u64 = 1;

/// `TIMER_ABSTIME`: `req` is an absolute deadline on `clock` rather than a
/// duration.
const TIMER_ABSTIME: u64 = 1;

/// Milliseconds to 100 Hz PIT ticks, rounding up so a positive timeout never
/// fires early.
pub(super) fn millis_to_ticks(millis: u64) -> u64 {
    millis.div_ceil(10).max(1)
}

/// Monotonic tick count from the PIT (100 Hz).
fn now_ticks() -> u64 {
    crate::arch::idt::TICKS.load(core::sync::atomic::Ordering::Relaxed)
}

/// `CLOCK_MONOTONIC_RAW`, `CLOCK_MONOTONIC_COARSE` and `CLOCK_BOOTTIME`: the
/// same boot-relative clock here (nothing is slewed, and suspend does not
/// exist).
const MONOTONIC_ALIASES: [u64; 3] = [4, 6, 7];

pub(super) fn sys_clock_gettime(clock: u64, out: u64) -> u64 {
    // The monotonic clocks count from boot with the TSC's sub-tick
    // resolution (`arch::clock`); everything else is wall time.
    let (seconds, nanos) = if clock == CLOCK_MONOTONIC || MONOTONIC_ALIASES.contains(&clock) {
        let ns = crate::arch::clock::monotonic_ns();
        ((ns / 1_000_000_000) as i64, (ns % 1_000_000_000) as u32)
    } else {
        crate::wallclock::now_ns()
    };
    write_timespec(out, seconds as u64, u64::from(nanos));
    0
}

fn write_timespec(out: u64, sec: u64, nsec: u64) {
    // Safety: user buffer holds a `struct timespec` (the syscall ABI's contract).
    unsafe {
        user_ptr::write::<i64>(out, sec as i64);
        user_ptr::write::<i64>(out + 8, nsec as i64);
    }
}

pub(super) fn sys_clock_getres(out: u64) -> u64 {
    // 1 ns with a calibrated TSC, the 10 ms tick without one.
    write_timespec(out, 0, crate::arch::clock::resolution_ns());
    0
}

pub(super) fn sys_gettimeofday(tv: u64) -> u64 {
    let (seconds, nanos) = crate::wallclock::now_ns();
    // Safety: user buffer holds a `struct timeval` (the syscall ABI's contract).
    unsafe {
        user_ptr::write::<i64>(tv, seconds);
        user_ptr::write::<i64>(tv + 8, i64::from(nanos / 1000));
    }
    0
}

/// `clock_settime(clock, ts)`: step `CLOCK_REALTIME`. Needs `CAP_SYS_TIME`;
/// `CLOCK_MONOTONIC` cannot be set, and a malformed or pre-epoch timespec is
/// `-EINVAL`. The capability is checked first so an unprivileged caller
/// learns nothing about which arguments would have been valid.
pub(super) fn sys_clock_settime(clock: u64, ts: u64) -> u64 {
    if !credentials::of(task::current()).has_cap(CAP_SYS_TIME) {
        return err(EPERM);
    }
    if clock != CLOCK_REALTIME {
        return err(EINVAL);
    }
    // Safety: user buffer holds a `struct timespec` (the syscall ABI's contract).
    let (sec, nsec) = unsafe { (user_ptr::read::<i64>(ts), user_ptr::read::<i64>(ts + 8)) };
    if !(0..crate::wallclock::MAX_SET_SECS).contains(&sec) || !(0..1_000_000_000).contains(&nsec) {
        return err(EINVAL);
    }
    crate::wallclock::set(sec, (nsec / 10_000_000) as u32);
    0
}

/// Sleep for the `struct timespec` at `req`, backing both `nanosleep` (always
/// relative, clock-independent) and `clock_nanosleep` (relative or, with
/// `TIMER_ABSTIME`, an absolute deadline on `clock`).
///
/// `clock` must be `CLOCK_REALTIME` or `CLOCK_MONOTONIC`, `flags` must
/// contain no bits beyond `TIMER_ABSTIME`, and `req`'s nanoseconds must be a
/// canonical `0..1_000_000_000` — matching Linux's `-EINVAL` for an unknown
/// clock, unknown flags, or a malformed timespec.
pub(super) fn sys_clock_nanosleep(clock: u64, flags: u64, req: u64, rem: u64) -> u64 {
    if clock != CLOCK_REALTIME && clock != CLOCK_MONOTONIC {
        return err(EINVAL);
    }
    if flags & !TIMER_ABSTIME != 0 {
        return err(EINVAL);
    }
    let absolute = flags & TIMER_ABSTIME != 0;
    // Safety: user buffer holds a `struct timespec` (the syscall ABI's contract).
    let (sec, nsec) = unsafe { (user_ptr::read::<i64>(req), user_ptr::read::<i64>(req + 8)) };
    if sec < 0 || !(0..1_000_000_000).contains(&nsec) {
        return err(EINVAL);
    }
    // 100 Hz timer: round up to whole ticks, at least one so time advances.
    // The sleep queue is never notified; the timer's deadline sweep is what
    // makes this return, exactly like a timeout.
    if absolute && clock == CLOCK_REALTIME {
        return sleep_until_realtime(sec as u64, nsec as u64);
    }
    let deadline = if absolute {
        clock_deadline_ticks(clock, sec as u64, nsec as u64)
    } else {
        let millis = (sec as u64)
            .saturating_mul(1000)
            .saturating_add((nsec as u64).div_ceil(1_000_000));
        now_ticks().saturating_add(millis_to_ticks(millis))
    };
    match task::wait_sleep(deadline) {
        WakeReason::TimedOut => 0,
        WakeReason::Interrupted => {
            // TIMER_ABSTIME sleeps never report a remainder (there's nothing
            // to resume relative to); only a relative sleep does.
            if !absolute && rem != 0 {
                let remaining = deadline.saturating_sub(now_ticks());
                write_timespec(rem, remaining / 100, (remaining % 100) * 10_000_000);
            }
            err(EINTR)
        }
        WakeReason::Woken => 0, // nothing notifies the sleep queue
    }
}

/// Longest single wait of an absolute `CLOCK_REALTIME` sleep, in ticks. The
/// wall clock can be stepped while a task sleeps, and nothing wakes sleepers
/// when it is, so the deadline is re-derived from the wall clock this often.
const REALTIME_RECHECK_TICKS: u64 = 100;

/// Sleep until the wall clock reaches `(sec, nsec)`, following any
/// `clock_settime` step in either direction within `REALTIME_RECHECK_TICKS`.
fn sleep_until_realtime(sec: u64, nsec: u64) -> u64 {
    loop {
        let target = clock_deadline_ticks(CLOCK_REALTIME, sec, nsec.min(999_999_999));
        let now = now_ticks();
        if target <= now {
            return 0;
        }
        if let WakeReason::Interrupted = task::wait_sleep(target.min(now + REALTIME_RECHECK_TICKS))
        {
            return err(EINTR);
        }
    }
}

/// Convert an absolute `(sec, nsec)` deadline on `clock`, expressed exactly as
/// `clock_gettime` reports that clock, into the PIT tick count `wait_sleep`
/// compares against. Rounds up so a sleeper never wakes before the requested
/// instant; a deadline already in the past saturates to tick 0, which
/// `wait_sleep` resolves immediately since ticks only advance.
fn clock_deadline_ticks(clock: u64, sec: u64, nsec: u64) -> u64 {
    let centis = nsec.div_ceil(10_000_000);
    if clock == CLOCK_MONOTONIC {
        sec.saturating_mul(100).saturating_add(centis)
    } else {
        crate::wallclock::wall_to_ticks(sec, centis)
    }
}

/// Most bytes one `getrandom` call fills. A short read is legal (callers
/// loop), and the cap bounds the time spent with interrupts off.
const GETRANDOM_MAX: u64 = 4096;

pub(super) fn sys_getrandom(buf: u64, len: u64) -> u64 {
    let len = len.min(GETRANDOM_MAX);
    let mut chunk = [0u8; 256];
    let mut written = 0u64;
    while written < len {
        let n = ((len - written) as usize).min(chunk.len());
        fill_random(&mut chunk[..n]);
        let Some(dest) = buf.checked_add(written) else {
            break;
        };
        if user_ptr::try_copy_to(dest, &chunk[..n]).is_err() {
            // Nothing delivered yet is a fault; otherwise report the short read.
            return if written == 0 { err(EFAULT) } else { written };
        }
        written += n as u64;
    }
    written
}
