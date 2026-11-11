//! [`Deadline`]: how long a call may take.

use core::time::Duration;

/// How long a call (or a wait for a name) may take, as the kernel call ABI
/// distinguishes it: not at all, a bounded time, or for ever. A relative
/// bound, so every runtime turns it into its own clock's absolute value
/// ([`Deadline::absolute_ticks`] for the PIT the syscalls use).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Deadline {
    /// Until the peer answers. Never on a UI thread.
    Never,
    /// Already expired: the call is answered only if the callee can do so in
    /// its current turn (a topic pull with nothing queued), else it fails
    /// with `ETIMEDOUT` at once. For a name wait: one attempt.
    Poll,
    /// At most this long.
    Within(Duration),
}

/// Length of one PIT tick in milliseconds (100 Hz).
pub const TICK_MILLIS: u64 = 10;

/// The absolute deadline the kernel reads as "poll"
/// (`lazyos_sys::msg::EXPIRED_DEADLINE`).
const POLL_TICK: u64 = 1;

impl Deadline {
    /// At most `millis` milliseconds.
    pub const fn millis(millis: u64) -> Deadline {
        Deadline::Within(Duration::from_millis(millis))
    }

    /// At most `secs` seconds.
    pub const fn secs(secs: u64) -> Deadline {
        Deadline::Within(Duration::from_secs(secs))
    }

    /// The absolute PIT tick the call ABI takes, `now` being the current
    /// tick: `0` waits for ever, `1` polls, and a bound is at least one tick
    /// and never collides with those two values.
    pub fn absolute_ticks(self, now: u64) -> u64 {
        match self {
            Deadline::Never => 0,
            Deadline::Poll => POLL_TICK,
            Deadline::Within(limit) => {
                let millis = u64::try_from(limit.as_millis()).unwrap_or(u64::MAX);
                let ticks = millis.div_ceil(TICK_MILLIS).max(1);
                now.saturating_add(ticks).max(POLL_TICK + 1)
            }
        }
    }

    /// The bound as a [`Duration`] (`None` for [`Deadline::Never`], zero for
    /// [`Deadline::Poll`]).
    pub fn limit(self) -> Option<Duration> {
        match self {
            Deadline::Never => None,
            Deadline::Poll => Some(Duration::ZERO),
            Deadline::Within(limit) => Some(limit),
        }
    }
}
