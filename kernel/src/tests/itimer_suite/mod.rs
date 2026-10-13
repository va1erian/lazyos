//! Interval and POSIX timers (issue #535, gap 3): `alarm`, `setitimer`,
//! `getitimer` and `timer_*`, through the real syscall dispatch, with the
//! clock driven by hand (`itimer::fire_due(now)`) so no test waits.
//!
//! The processes are forked children of the kernel task with every signal
//! blocked, so a timer's signal stays *pending* and can be read back without
//! the sweep acting on it; delivery to a running program (a handler, an
//! interrupted `read`) is the ABI fixture's job (`tools/abi`, `alarm`).

use super::*;
use crate::task::itimer;
use crate::task::signal::{self, SigInfo};

mod interval;
mod posix;
mod soak;

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "itimer_alarm_remaining_and_cancel",
        interval::alarm_remaining,
    ),
    ("itimer_alarm_fires_once", interval::alarm_fires_once),
    (
        "itimer_setitimer_validation",
        interval::setitimer_validation,
    ),
    (
        "itimer_interval_reloads_and_skips",
        interval::interval_reloads,
    ),
    ("itimer_cpu_timers_count_ticks", interval::cpu_timers),
    ("itimer_not_inherited_by_fork", interval::not_inherited),
    (
        "itimer_exec_keeps_intervals_drops_posix",
        interval::exec_semantics,
    ),
    (
        "itimer_scheduler_entry_fires_due",
        interval::scheduler_entry_fires,
    ),
    ("itimer_posix_create_and_siginfo", posix::create_and_siginfo),
    (
        "itimer_posix_settime_gettime_overrun",
        posix::settime_gettime_overrun,
    ),
    (
        "itimer_posix_absolute_and_clocks",
        posix::absolute_and_clocks,
    ),
    ("itimer_posix_argument_validation", posix::validation),
    ("itimer_posix_limit_and_delete", posix::limit_and_delete),
    ("itimer_soak_arm_disarm", soak::arm_disarm),
    ("itimer_soak_interval_many_ticks", soak::many_ticks),
    ("itimer_soak_fork_exit_churn", soak::fork_exit_churn),
];

pub(super) const NS: u64 = 1_000_000_000;

pub(super) const SYS_GETITIMER: u64 = 36;
pub(super) const SYS_ALARM: u64 = 37;
pub(super) const SYS_SETITIMER: u64 = 38;
pub(super) const SYS_TIMER_CREATE: u64 = 222;
pub(super) const SYS_TIMER_SETTIME: u64 = 223;
pub(super) const SYS_TIMER_GETTIME: u64 = 224;
pub(super) const SYS_TIMER_GETOVERRUN: u64 = 225;
pub(super) const SYS_TIMER_DELETE: u64 = 226;

pub(super) const EAGAIN: u64 = (-11i64) as u64;
pub(super) const EINVAL: u64 = (-22i64) as u64;

/// Everything blocked: timers' signals wait in the pending sets.
const ALL_BLOCKED: u64 = u64::MAX;

/// A fresh single-thread process (a forked child of the kernel task, a Linux
/// task with a kernel frame) that is the current task, every signal blocked.
/// The caller hands the slot to [`finish`].
pub(super) fn process() -> Result<usize, String> {
    let slot = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    task::harness::set_kind(slot, task::Kind::Linux);
    task::harness::set_kernel_frame(slot).ok_or("the child has no frame")?;
    signal::set_blocked(slot, ALL_BLOCKED);
    task::harness::switch_current(slot);
    Ok(slot)
}

/// Back to the kernel task; end and reap `slots`.
pub(super) fn finish(slots: &[usize]) -> Result<(), String> {
    task::harness::switch_current(task::KERNEL_TASK);
    for &slot in slots {
        task::harness::finish(slot, 0);
    }
    let mut reaped = 0;
    while task::reap_child().is_some() {
        reaped += 1;
    }
    check!(
        reaped == slots.len(),
        "reaped {reaped} of {} children",
        slots.len()
    );
    Ok(())
}

/// Start state: the kernel task, no timers, an empty signal registry.
pub(super) fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    signal::harness::reset();
    itimer::reset_for_test();
    check!(
        itimer::in_use() == 0,
        "timers left over: {}",
        itimer::in_use()
    );
    Ok(())
}

pub(super) fn now() -> u64 {
    crate::arch::clock::monotonic_ns()
}

pub(super) fn bit(sig: u8) -> u64 {
    1u64 << sig
}

/// Signals pending for `slot` (kernel bit order).
pub(super) fn pending(slot: usize) -> u64 {
    signal::pending(slot)
}

/// A `struct itimerval`/`itimerspec` as user memory: interval then value,
/// each `(sec, frac)`.
pub(super) fn spec(interval: (i64, i64), value: (i64, i64)) -> [i64; 4] {
    [interval.0, interval.1, value.0, value.1]
}

pub(super) fn ptr<T>(value: &T) -> u64 {
    value as *const T as u64
}

pub(super) fn ptr_mut<T>(value: &mut T) -> u64 {
    value as *mut T as u64
}

/// `getitimer(which)` as `(remaining_ns, interval_ns)`.
pub(super) fn getitimer(which: u64) -> Result<(u64, u64), String> {
    let mut out = [0i64; 4];
    let ret = process::linux::dispatch_for_test(SYS_GETITIMER, which, ptr_mut(&mut out), 0);
    check!(ret == 0, "getitimer({which}) returned {ret:#x}");
    let ns = |sec: i64, usec: i64| sec as u64 * NS + usec as u64 * 1000;
    Ok((ns(out[2], out[3]), ns(out[0], out[1])))
}

pub(super) fn setitimer(which: u64, new: &[i64; 4], old: Option<&mut [i64; 4]>) -> u64 {
    let old = old.map_or(0, ptr_mut);
    process::linux::dispatch_for_test(SYS_SETITIMER, which, ptr(new), old)
}

pub(super) fn info_of(sig: u8) -> Result<SigInfo, String> {
    signal::harness::recorded_info(sig).ok_or_else(|| "no recorded siginfo".into())
}
