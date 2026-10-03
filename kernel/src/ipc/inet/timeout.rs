//! `SO_RCVTIMEO` and `SO_SNDTIMEO`: how long a blocking call on a socket may
//! wait before it gives up (docs/tls-plan.md §5.4).
//!
//! A timeout is kept in scheduler ticks, as Linux keeps it in jiffies: the
//! socket-option layer converts a `struct timeval` once, rounding up, and
//! `getsockopt` reports the stored value back. Each blocking call turns it
//! into an absolute deadline when it starts, so wake-ups that find nothing to
//! do do not extend the wait.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::task;

/// The stored form of "no timeout" (Linux's `MAX_SCHEDULE_TIMEOUT`).
const FOREVER: u64 = u64::MAX;

/// Which of a socket's two timeouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// `SO_RCVTIMEO`: `recv`, `read`, `accept`.
    Recv,
    /// `SO_SNDTIMEO`: `send`, `write`, `connect`.
    Send,
}

/// One socket's two timeouts. `None` waits for ever; `Some(0)` gives up at
/// once (Linux's answer to a negative `tv_sec`).
pub(super) struct Timeouts {
    recv: AtomicU64,
    send: AtomicU64,
}

impl Timeouts {
    pub(super) const fn new() -> Timeouts {
        Timeouts {
            recv: AtomicU64::new(FOREVER),
            send: AtomicU64::new(FOREVER),
        }
    }

    fn cell(&self, dir: Dir) -> &AtomicU64 {
        match dir {
            Dir::Recv => &self.recv,
            Dir::Send => &self.send,
        }
    }

    /// The timeout in ticks, `None` for no timeout.
    pub(super) fn get(&self, dir: Dir) -> Option<u64> {
        let ticks = self.cell(dir).load(Ordering::Acquire);
        (ticks != FOREVER).then_some(ticks)
    }

    /// Store a timeout in ticks; a value of [`FOREVER`] or more means none.
    pub(super) fn set(&self, dir: Dir, ticks: Option<u64>) {
        self.cell(dir)
            .store(ticks.unwrap_or(FOREVER), Ordering::Release);
    }

    /// The absolute tick a call starting now must give up at.
    pub(super) fn deadline(&self, dir: Dir) -> Option<u64> {
        self.get(dir)
            .map(|ticks| task::ticks().saturating_add(ticks))
    }
}
