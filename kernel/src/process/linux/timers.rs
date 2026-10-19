//! Interval and POSIX timers (issue #535, gap 3): `alarm`, `setitimer`,
//! `getitimer` and `timer_create`/`timer_settime`/`timer_gettime`/
//! `timer_getoverrun`/`timer_delete`.
//!
//! This module is only the ABI: decoding `struct itimerval`, `itimerspec`
//! and `sigevent`, and the errno mapping. The timers themselves, how they
//! are driven and what is approximate (the CPU-time ones) are in
//! [`crate::task::itimer`]; a timer's signal is an ordinary signal, so a
//! blocking call it interrupts ends with `EINTR` (or restarts under
//! `SA_RESTART`) by the rules of [`super::restartable`].

use crate::ipc::credentials::{self, CAP_SYS_ADMIN};
use crate::task;
use crate::task::itimer::{self, Caller, Kind, Notify, Reading};
use crate::task::signal;
use crate::user_ptr;

use super::errno::{err, EAGAIN, EFAULT, EINVAL};
use super::time::{clock_deadline_ns, now_ns, CLOCK_MONOTONIC, CLOCK_REALTIME};

const NS_PER_SEC: u64 = 1_000_000_000;
const NS_PER_USEC: u64 = 1_000;

const ITIMER_REAL: u64 = 0;
const ITIMER_VIRTUAL: u64 = 1;
const ITIMER_PROF: u64 = 2;

const SIGEV_SIGNAL: i32 = 0;
const SIGEV_NONE: i32 = 1;
const SIGEV_THREAD_ID: i32 = 4;

/// `CLOCK_BOOTTIME`: this kernel's monotonic clock under another name.
const CLOCK_BOOTTIME: u64 = 7;

const TIMER_ABSTIME: u64 = 1;

/// The calling process's address space (the key timers are kept under).
fn process() -> u64 {
    task::pml4_of(task::current()).unwrap_or(0)
}

/// The process's address space, with its signal state created: a timer
/// signals from an interrupt, where the registry must not grow.
fn arming_process() -> u64 {
    let pml4 = process();
    signal::ensure_process(pml4);
    pml4
}

/// The caller's quota identity: its uid, and whether it may arm timers
/// faster and in greater number than a program with no capability.
fn caller() -> Caller {
    let cred = credentials::of(task::current());
    Caller {
        uid: cred.uid,
        privileged: cred.has_cap(CAP_SYS_ADMIN),
    }
}

fn kind_of(which: u64) -> Option<Kind> {
    match which {
        ITIMER_REAL => Some(Kind::Real),
        ITIMER_VIRTUAL => Some(Kind::Virtual),
        ITIMER_PROF => Some(Kind::Prof),
        _ => None,
    }
}

fn timer_error(error: itimer::Error) -> u64 {
    match error {
        itimer::Error::NoSpace => err(EAGAIN),
        itimer::Error::NoSuchTimer => err(EINVAL),
    }
}

/// A user `timeval`/`timespec` pair as nanoseconds: `sec` non-negative and
/// the fraction below one second (`unit` ns per fraction step), else `None`.
fn read_time(addr: u64, unit: u64) -> Option<Result<u64, u64>> {
    let (Ok(sec), Ok(frac)) = (
        user_ptr::try_read::<i64>(addr),
        user_ptr::try_read_field::<i64>(addr, 8),
    ) else {
        return Some(Err(err(EFAULT)));
    };
    let limit = (NS_PER_SEC / unit) as i64;
    if sec < 0 || !(0..limit).contains(&frac) {
        return None;
    }
    Some(Ok((sec as u64)
        .saturating_mul(NS_PER_SEC)
        .saturating_add(frac as u64 * unit)))
}

/// Write `ns` as a `timeval`/`timespec` pair at `addr`.
fn write_time(addr: u64, ns: u64, unit: u64) -> Result<(), u64> {
    let sec = (ns / NS_PER_SEC) as i64;
    let frac = ((ns % NS_PER_SEC) / unit) as i64;
    user_ptr::try_write::<i64>(addr, sec).map_err(|_| err(EFAULT))?;
    user_ptr::try_write_field::<i64>(addr, 8, frac).map_err(|_| err(EFAULT))
}

/// An `itimerval`/`itimerspec` (interval at 0, value at 16) from user space:
/// `(value_ns, interval_ns)`.
fn read_spec(addr: u64, unit: u64) -> Result<(u64, u64), u64> {
    let interval = read_time(addr, unit).ok_or(err(EINVAL))??;
    let value = read_time(user_ptr::at(addr, 16), unit).ok_or(err(EINVAL))??;
    Ok((value, interval))
}

fn write_spec(addr: u64, reading: Reading, unit: u64) -> Result<(), u64> {
    write_time(addr, reading.interval_ns, unit)?;
    write_time(user_ptr::at(addr, 16), reading.remaining_ns, unit)
}

/// `alarm(seconds)`: arm `ITIMER_REAL` once (0 cancels) and return the whole
/// seconds the previous alarm had left, rounded up.
pub(super) fn sys_alarm(seconds: u64) -> u64 {
    let seconds = seconds as u32 as u64;
    match itimer::set_interval(
        arming_process(),
        task::current(),
        caller(),
        Kind::Real,
        now_ns(),
        seconds * NS_PER_SEC,
        0,
    ) {
        Ok(old) => old.remaining_ns.div_ceil(NS_PER_SEC),
        // `alarm` cannot fail: the table is full only under a flood.
        Err(_) => 0,
    }
}

/// `setitimer(which, new, old)`.
pub(super) fn sys_setitimer(which: u64, new: u64, old: u64) -> u64 {
    let Some(kind) = kind_of(which) else {
        return err(EINVAL);
    };
    // A NULL `new` is a zero `itimerval`: the timer is disarmed (Linux).
    let (value, interval) = if new == 0 {
        (0, 0)
    } else {
        match read_spec(new, NS_PER_USEC) {
            Ok(spec) => spec,
            Err(errno) => return errno,
        }
    };
    let previous = match itimer::set_interval(
        arming_process(),
        task::current(),
        caller(),
        kind,
        now_ns(),
        value,
        interval,
    ) {
        Ok(previous) => previous,
        Err(error) => return timer_error(error),
    };
    if old != 0 {
        if let Err(errno) = write_spec(old, previous, NS_PER_USEC) {
            return errno;
        }
    }
    0
}

/// `getitimer(which, cur)`.
pub(super) fn sys_getitimer(which: u64, current: u64) -> u64 {
    let Some(kind) = kind_of(which) else {
        return err(EINVAL);
    };
    let reading = itimer::get_interval(process(), kind, now_ns());
    match write_spec(current, reading, NS_PER_USEC) {
        Ok(()) => 0,
        Err(errno) => errno,
    }
}

/// Whether `signo` may be a timer's signal: a real signal number, and not
/// one that acts on the whole process from the interrupt that sends it
/// (`SIGKILL`, `SIGSTOP`) or is job control (`SIGCONT` and the tty stops).
fn timer_signal_ok(signo: i32) -> bool {
    let Ok(signo) = u8::try_from(signo) else {
        return false;
    };
    signo != 0
        && (signo as usize) < signal::NSIG
        && !matches!(
            signo,
            signal::SIGKILL
                | signal::SIGSTOP
                | signal::SIGCONT
                | signal::SIGTSTP
                | signal::SIGTTIN
                | signal::SIGTTOU
        )
}

/// `timer_create(clockid, sevp, timerid)`.
///
/// `SIGEV_SIGNAL`, `SIGEV_THREAD_ID` and `SIGEV_NONE` are accepted (musl's
/// `SIGEV_THREAD` is built on `SIGEV_THREAD_ID`); `sevp == NULL` means
/// `SIGEV_SIGNAL` with `SIGALRM` and the timer id as the value. Only
/// `CLOCK_REALTIME`, `CLOCK_MONOTONIC` and `CLOCK_BOOTTIME` are timer clocks
/// (the CPU-time clocks are `EINVAL`).
pub(super) fn sys_timer_create(clock: u64, sevp: u64, timerid: u64) -> u64 {
    if clock != CLOCK_REALTIME && clock != CLOCK_MONOTONIC && clock != CLOCK_BOOTTIME {
        return err(EINVAL);
    }
    let me = task::current();
    let pml4 = arming_process();
    let (notify, signo, value) = if sevp == 0 {
        // Without a sigevent the value is the timer id (Linux).
        (Notify::Process, signal::SIGALRM, None)
    } else {
        let (Ok(value), Ok(signo), Ok(mode), Ok(tid)) = (
            user_ptr::try_read::<u64>(sevp),
            user_ptr::try_read_field::<i32>(sevp, 8),
            user_ptr::try_read_field::<i32>(sevp, 12),
            user_ptr::try_read_field::<i32>(sevp, 16),
        ) else {
            return err(EFAULT);
        };
        let notify = match mode {
            SIGEV_NONE => Notify::None,
            SIGEV_SIGNAL => Notify::Process,
            SIGEV_THREAD_ID => {
                let tid = tid as usize;
                // The target must be a live thread of this process.
                if tid == 0 || task::pml4_of(tid) != Some(pml4) {
                    return err(EINVAL);
                }
                Notify::Thread {
                    slot: tid,
                    generation: signal::thread_generation(tid),
                }
            }
            _ => return err(EINVAL),
        };
        // Validated as the full `int` the user passed: 270 is not SIGALRM.
        if notify != Notify::None && !timer_signal_ok(signo) {
            return err(EINVAL);
        }
        (notify, signo as u8, Some(value))
    };
    let id = match itimer::create(
        pml4,
        me,
        caller(),
        notify,
        signo,
        value,
        // `CLOCK_BOOTTIME` is the monotonic clock here.
        if clock == CLOCK_REALTIME { 0 } else { 1 },
    ) {
        Ok(id) => id,
        Err(error) => return timer_error(error),
    };
    if user_ptr::try_write::<i32>(timerid, id).is_err() {
        let _ = itimer::delete(pml4, id);
        return err(EFAULT);
    }
    0
}

/// `timer_settime(timerid, flags, new, old)`.
pub(super) fn sys_timer_settime(id: u64, flags: u64, new: u64, old: u64) -> u64 {
    if flags & !TIMER_ABSTIME != 0 {
        return err(EINVAL);
    }
    if new == 0 {
        return err(EFAULT);
    }
    let (value, interval) = match read_spec(new, 1) {
        Ok(spec) => spec,
        Err(errno) => return errno,
    };
    let pml4 = process();
    let id = id as i32;
    let Ok(clock) = itimer::clock_of(pml4, id) else {
        return err(EINVAL);
    };
    let now = now_ns();
    let start = if value == 0 {
        None
    } else if flags & TIMER_ABSTIME != 0 {
        Some(clock_deadline_ns(
            clock as u64,
            value / NS_PER_SEC,
            value % NS_PER_SEC,
        ))
    } else {
        Some(now.saturating_add(value))
    };
    match itimer::settime(pml4, id, now, start, interval) {
        Ok(previous) => {
            if old != 0 {
                if let Err(errno) = write_spec(old, previous, 1) {
                    return errno;
                }
            }
            0
        }
        Err(error) => timer_error(error),
    }
}

/// `timer_gettime(timerid, cur)`.
pub(super) fn sys_timer_gettime(id: u64, current: u64) -> u64 {
    match itimer::gettime(process(), id as i32, now_ns()) {
        Ok(reading) => match write_spec(current, reading, 1) {
            Ok(()) => 0,
            Err(errno) => errno,
        },
        Err(error) => timer_error(error),
    }
}

/// `timer_getoverrun(timerid)`.
pub(super) fn sys_timer_getoverrun(id: u64) -> u64 {
    match itimer::overrun(process(), id as i32) {
        Ok(count) => u64::from(count),
        Err(error) => timer_error(error),
    }
}

/// `timer_delete(timerid)`.
pub(super) fn sys_timer_delete(id: u64) -> u64 {
    match itimer::delete(process(), id as i32) {
        Ok(()) => 0,
        Err(error) => timer_error(error),
    }
}
