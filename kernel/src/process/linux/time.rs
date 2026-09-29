//! Clocks and sleeping: `gettimeofday`, `clock_gettime`/`clock_getres`,
//! `nanosleep`/`clock_nanosleep`, and `getrandom`. [`millis_to_ticks`] is
//! shared with the poll-style waits in [`super::io`] and [`super::epoll`],
//! since a millisecond timeout is the common currency across all of them.

use crate::task::{self, WakeReason};
use crate::user_ptr;

use super::errno::{err, EFAULT, EINTR, EINVAL};
use super::uaccess::fill_random;

/// Fixed realtime epoch (2026-01-01T00:00:00Z); the PIT provides monotonicity.
const REALTIME_BASE: u64 = 1_767_225_600;

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

pub(super) fn sys_clock_gettime(clock: u64, out: u64) -> u64 {
    // CLOCK_MONOTONIC counts from boot; everything else is anchored to epoch.
    let ticks = now_ticks();
    let seconds = if clock == CLOCK_MONOTONIC {
        ticks / 100
    } else {
        REALTIME_BASE + ticks / 100
    };
    write_timespec(out, seconds, (ticks % 100) * 10_000_000);
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
    // 100 Hz PIT => 10 ms resolution.
    write_timespec(out, 0, 10_000_000);
    0
}

pub(super) fn sys_gettimeofday(tv: u64) -> u64 {
    let ticks = now_ticks();
    // Safety: user buffer holds a `struct timeval` (the syscall ABI's contract).
    unsafe {
        user_ptr::write::<i64>(tv, (REALTIME_BASE + ticks / 100) as i64);
        user_ptr::write::<i64>(tv + 8, ((ticks % 100) * 10_000) as i64);
    }
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

/// Convert an absolute `(sec, nsec)` deadline on `clock`, expressed exactly as
/// `clock_gettime` reports that clock, into the PIT tick count `wait_sleep`
/// compares against. Rounds up so a sleeper never wakes before the requested
/// instant; a deadline already in the past saturates to tick 0, which
/// `wait_sleep` resolves immediately since ticks only advance.
fn clock_deadline_ticks(clock: u64, sec: u64, nsec: u64) -> u64 {
    let ticks = sec
        .saturating_mul(100)
        .saturating_add(nsec.div_ceil(10_000_000));
    if clock == CLOCK_MONOTONIC {
        ticks
    } else {
        ticks.saturating_sub(REALTIME_BASE * 100)
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
