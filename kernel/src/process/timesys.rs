//! Native monotonic-time syscall 34 (docs/performance-plan.md P2.4): the
//! nanosecond clock and a nanosecond sleep, so a native program can nap for
//! a millisecond without reaping a child (`wait`) or building a channel pair
//! to park on.
//!
//! | `rdi` op | call | `rsi` | result |
//! |----------|------|-------|--------|
//! | 0 | `now` | - | `arch::clock::monotonic_ns` |
//! | 1 | `sleep_until` | deadline (monotonic ns) | 0 when it passed, 1 when a signal interrupted the sleep |
//!
//! The deadline is absolute, so a loop that sleeps to a schedule does not
//! drift; a deadline already past returns at once. The 100 Hz tick ABI
//! (`clock`, syscall 8, and every tick deadline) is unchanged.

use crate::task::{self, WakeReason};

/// Native monotonic-time ops (syscall 34).
pub mod op {
    /// Read the monotonic clock, nanoseconds since boot.
    pub const NOW: u64 = 0;
    /// Sleep until a monotonic deadline.
    pub const SLEEP_UNTIL: u64 = 1;
}

/// `sleep_until`'s result when the deadline passed.
pub const ELAPSED: u64 = 0;
/// `sleep_until`'s result when a signal ended the sleep early.
pub const INTERRUPTED: u64 = 1;

const EINVAL: i64 = 22;

/// Route one syscall-34 call. Runs with interrupts off (the native gate).
pub fn dispatch(operation: u64, arg: u64) -> u64 {
    match operation {
        op::NOW => crate::arch::clock::monotonic_ns(),
        op::SLEEP_UNTIL => sleep_until(arg),
        _ => EINVAL.wrapping_neg() as u64,
    }
}

fn sleep_until(deadline: u64) -> u64 {
    loop {
        match task::wait_sleep_ns(deadline) {
            WakeReason::TimedOut => return ELAPSED,
            WakeReason::Interrupted => return INTERRUPTED,
            // Nothing notifies the sleep queue; a stray wake sleeps on.
            WakeReason::Woken => {}
        }
    }
}
