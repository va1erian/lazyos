//! The clocks: the PIT tick counter (syscall 8) that Messenger deadlines are
//! absolute values of, the monotonic nanosecond clock and sleep (34, see
//! `kernel/src/process/timesys.rs`), and the UTC wall clock (24).

use crate::nr;

/// PIT ticks per second.
pub const TICK_HZ: u64 = 100;
/// One PIT tick in milliseconds.
pub const TICK_MS: u64 = 1000 / TICK_HZ;
/// One PIT tick in nanoseconds.
pub const TICK_NS: u64 = 1_000_000_000 / TICK_HZ;

const MONO_NOW: u64 = 0;
const MONO_SLEEP_UNTIL: u64 = 1;
/// `sleep_until`'s result when a signal ended the sleep.
const MONO_INTERRUPTED: u64 = 1;

const WALL_GET: u64 = 0;
const WALL_SET: u64 = 1;

/// The PIT tick counter (100 Hz); `0` before the first tick. Messenger
/// deadlines and the child-exit [`crate::process::wait`] are absolute
/// values of this clock.
pub fn clock() -> u64 {
    crate::raw::syscall0(nr::CLOCK) as u64
}

/// The absolute tick `ms` milliseconds from now (at least one tick ahead),
/// for a Messenger deadline.
pub fn deadline_after_ms(ms: u64) -> u64 {
    clock().saturating_add(ms.div_ceil(TICK_MS).max(1))
}

fn mono_time(op: u64, arg: u64) -> u64 {
    // SAFETY: syscall 34 takes no pointer.
    unsafe { crate::raw::syscall2(nr::MONO_TIME, op, arg) as u64 }
}

/// Nanoseconds since boot on the monotonic clock (the Linux
/// `CLOCK_MONOTONIC`), with sub-tick resolution.
pub fn monotonic_ns() -> u64 {
    mono_time(MONO_NOW, 0)
}

/// Milliseconds since boot on the monotonic clock.
pub fn monotonic_ms() -> u64 {
    monotonic_ns() / 1_000_000
}

/// Sleep until [`monotonic_ns`] reaches `deadline`. Returns `false` when a
/// signal ended the sleep early.
pub fn sleep_until_ns(deadline: u64) -> bool {
    mono_time(MONO_SLEEP_UNTIL, deadline) != MONO_INTERRUPTED
}

/// Sleep for `ns` nanoseconds. Returns `false` when a signal ended the
/// sleep early.
pub fn sleep_ns(ns: u64) -> bool {
    sleep_until_ns(monotonic_ns().saturating_add(ns))
}

/// Sleep for `ms` milliseconds on the native clock (any task may: a musl
/// program needs no Linux `nanosleep` for it).
pub fn sleep_ms(ms: u64) -> bool {
    sleep_ns(ms.saturating_mul(1_000_000))
}

/// Nap one tick's worth (10 ms) in a deadline-bounded retry or poll loop.
pub fn nap() {
    sleep_ns(TICK_NS);
}

/// UTC centiseconds since the Unix epoch.
pub fn wall_centis() -> u64 {
    // SAFETY: syscall 24 takes no pointer.
    unsafe { crate::raw::syscall2(nr::WALL_TIME, WALL_GET, 0) as u64 }
}

/// Step the wall clock to `unix_secs` (UTC). Needs `CAP_SYS_TIME`; returns
/// the negative errno the kernel refused with.
pub fn wall_set(unix_secs: u64) -> Result<(), i64> {
    // SAFETY: syscall 24 takes no pointer.
    crate::zero(unsafe { crate::raw::syscall2(nr::WALL_TIME, WALL_SET, unix_secs) })
}

/// The monotonic deadline `after` from now, for [`crate::msg::wait_any_ns`].
#[cfg(feature = "std")]
pub fn deadline_after(after: std::time::Duration) -> u64 {
    let ns = u64::try_from(after.as_nanos()).unwrap_or(u64::MAX);
    monotonic_ns().saturating_add(ns)
}

/// [`sleep_ns`] for a [`std::time::Duration`].
#[cfg(feature = "std")]
pub fn sleep(duration: std::time::Duration) -> bool {
    sleep_ns(u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
}
