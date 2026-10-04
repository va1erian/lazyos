//! Keyed `poll`/`select` wakeups (docs/performance-plan.md P6.5).
//!
//! Every `poll`, `select` and `epoll` waiter parks on the one `wait::POLL`
//! queue. Before this, any pipe or pseudo-terminal event woke all of them,
//! and each rescanned its descriptors only to park again (a thundering herd).
//! Now a `poll` or `select` scan records which objects it looked at (its
//! *interest*: one key per pipe or pseudo-terminal, see `Fd::poll_keys`), and
//! a pipe or pseudo-terminal event wakes only the waiters interested in that
//! object ([`notify_poll_key`]).
//!
//! The rule that keeps this safe is that a waiter's interest can only narrow
//! its wakeups when it is complete:
//!
//! * a descriptor kind without a key (eventfds, sockets, listeners, epoll
//!   descriptors, the terminal, files) marks the interest *all*, and a full
//!   key list does too;
//! * a waiter that did not scan through [`fd_poll`](super::fd_poll) since
//!   [`scan_begin`] (`epoll_wait`, which watches its own interest list) waits
//!   with no interest recorded, which is *all*;
//! * notifications from objects without keys stay [`super::notify_poll`],
//!   which wakes everyone.
//!
//! The scan, the recording and the park all run in one syscall with
//! interrupts off on this single CPU, so no event can fall between a scan
//! that saw nothing ready and the park.

use spin::Mutex;

use super::{current, wait, WakeReason, MAX_TASKS};

/// Keys one waiter records before it falls back to "all".
const MAX_KEYS: usize = 16;

/// What one task's last scan looked at.
#[derive(Clone, Copy)]
struct Interest {
    /// Recording: set by [`scan_begin`], cleared by every unkeyed wait.
    active: bool,
    /// The scan met something without a key, or more than [`MAX_KEYS`].
    all: bool,
    len: usize,
    keys: [u64; MAX_KEYS],
}

impl Interest {
    const NONE: Interest = Interest {
        active: false,
        all: true,
        len: 0,
        keys: [0; MAX_KEYS],
    };

    /// Whether an event on `key` concerns this waiter.
    fn wants(&self, key: u64) -> bool {
        !self.active || self.all || self.keys[..self.len].contains(&key)
    }
}

static INTEREST: Mutex<[Interest; MAX_TASKS]> = Mutex::new([Interest::NONE; MAX_TASKS]);

/// The current task starts a scan whose interest [`wait_keyed_ns`] will use.
pub fn scan_begin() {
    if let Some(interest) = INTEREST.lock().get_mut(current()) {
        *interest = Interest {
            active: true,
            all: false,
            ..Interest::NONE
        };
    }
}

/// The scan looked at an object with these keys (`None`: a kind without
/// keys, which makes the interest "all").
pub(super) fn note(keys: Option<[u64; 2]>) {
    let mut table = INTEREST.lock();
    let Some(interest) = table.get_mut(current()) else {
        return;
    };
    if !interest.active || interest.all {
        return;
    }
    let Some(keys) = keys else {
        interest.all = true;
        return;
    };
    for key in keys.into_iter().filter(|&key| key != 0) {
        if interest.keys[..interest.len].contains(&key) {
            continue;
        }
        if interest.len == MAX_KEYS {
            interest.all = true;
            return;
        }
        interest.keys[interest.len] = key;
        interest.len += 1;
    }
}

/// Park on the poll queue until an event the last scan's interest covers, or
/// `deadline` (monotonic ns). For `poll` and `select`, right after a
/// [`scan_begin`] scan that found nothing ready.
pub fn wait_keyed_ns(deadline: Option<u64>) -> WakeReason {
    wait::POLL.wait_ns(current(), deadline)
}

/// Park on the poll queue for any event: a waiter that did not record an
/// interest (`epoll_wait`).
pub(super) fn wait_any_ns(deadline: Option<u64>) -> WakeReason {
    if let Some(interest) = INTEREST.lock().get_mut(current()) {
        *interest = Interest::NONE;
    }
    wait::POLL.wait_ns(current(), deadline)
}

/// An event on the object `key` (a pipe or a pseudo-terminal): wake the poll
/// waiters whose interest covers it.
pub fn notify_poll_key(key: u64) {
    wait::POLL.notify_matching(|slot| {
        INTEREST
            .lock()
            .get(slot)
            .is_none_or(|interest| interest.wants(key))
    });
    // A Messenger wait set parked on a descriptor (`WAIT_FD`, the desktop
    // Terminal on its pty master) records no keys: every pipe or pty event
    // reaches it, as every unkeyed `notify_poll` does.
    crate::ipc::channels::wake_fd_watchers();
}

/// Test hook: record `keys` in the current task's interest, as `fd_poll`
/// does for a descriptor during a scan.
#[cfg(lazyos_tests)]
pub fn note_for_test(keys: Option<[u64; 2]>) {
    note(keys);
}

/// Test hook: whether `slot`'s recorded interest covers `key`.
#[cfg(lazyos_tests)]
pub fn wants(slot: usize, key: u64) -> bool {
    INTEREST
        .lock()
        .get(slot)
        .is_none_or(|interest| interest.wants(key))
}

/// Forget a dead task's recorded poll interest (task teardown): its wait loop
/// never ran to reset it, and the slot may be reused.
pub fn forget_task(slot: usize) {
    if let Some(interest) = INTEREST.lock().get_mut(slot) {
        *interest = Interest::NONE;
    }
}
