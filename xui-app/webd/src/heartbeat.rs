//! The watchdog on WebKit's main thread. Every engine call is a task for the run loop
//! (`webd_engine_run_on_main`); if the loop stops turning (a page's script cannot do it, but a bug
//! in the engine's own code can), no view paints, loads or dies any more and nothing says so. The
//! service thread therefore posts a task that does nothing but answer, every [`PERIOD_MS`] while
//! there is a view, and a task that stays unanswered for [`LIMIT_MS`] means the engine is hung.
//!
//! The limit is long on purpose: the loop also paints (a 4K frame is tens of milliseconds) and
//! parses, and a heavily loaded machine runs everything late. A hang is not recoverable in place
//! (WebKit's main thread cannot be restarted), so the host answers it with
//! [`xui_webd::Service::engine_lost`] and ends the process; its clients see `ViewLost` and then
//! `EPIPE`, and start again. This module is the decision only: the clock and the answer are
//! the caller's, so it tests on any host.

/// Milliseconds between two probes while the service has views.
pub const PERIOD_MS: u64 = 2_000;
/// Milliseconds a probe may stay unanswered before the engine counts as hung.
pub const LIMIT_MS: u64 = 20_000;

/// What the caller does after a [`Heartbeat::poll`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Beat {
    /// Nothing to do.
    Idle,
    /// Post a probe task now.
    Send,
    /// The last probe has not been answered for [`LIMIT_MS`]: the engine is hung.
    Hung,
}

/// The watchdog's state.
#[derive(Debug, Default)]
pub struct Heartbeat {
    /// When the unanswered probe went out.
    outstanding_since: Option<u64>,
    /// When the next probe is due.
    next_ms: u64,
}

impl Heartbeat {
    pub fn new() -> Heartbeat {
        Heartbeat::default()
    }

    /// One look. `now_ms` is any monotonic clock; `answered` is whether the probe posted last
    /// time has run (the caller clears its flag when it sends); `busy` is whether there is a view,
    /// since an engine with no view has nothing to hang on.
    pub fn poll(&mut self, now_ms: u64, answered: bool, busy: bool) -> Beat {
        if let Some(since) = self.outstanding_since {
            if answered {
                self.outstanding_since = None;
                self.next_ms = now_ms.saturating_add(PERIOD_MS);
                return Beat::Idle;
            }
            return if now_ms.saturating_sub(since) >= LIMIT_MS {
                Beat::Hung
            } else {
                Beat::Idle
            };
        }
        if !busy || now_ms < self.next_ms {
            return Beat::Idle;
        }
        self.outstanding_since = Some(now_ms);
        Beat::Send
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_view_no_probe() {
        let mut beat = Heartbeat::new();
        assert_eq!(beat.poll(0, true, false), Beat::Idle);
        assert_eq!(beat.poll(100_000, true, false), Beat::Idle);
    }

    #[test]
    fn a_probe_goes_out_with_a_view_and_is_answered_before_the_next() {
        let mut beat = Heartbeat::new();
        assert_eq!(beat.poll(0, true, true), Beat::Send);
        // Not answered yet: wait, say nothing.
        assert_eq!(beat.poll(500, false, true), Beat::Idle);
        assert_eq!(beat.poll(1_000, true, true), Beat::Idle);
        // Answered at 1 s: the next is due at 3 s.
        assert_eq!(beat.poll(2_999, true, true), Beat::Idle);
        assert_eq!(beat.poll(3_000, true, true), Beat::Send);
    }

    #[test]
    fn a_probe_unanswered_for_the_limit_is_a_hang() {
        let mut beat = Heartbeat::new();
        assert_eq!(beat.poll(1_000, true, true), Beat::Send);
        assert_eq!(beat.poll(1_000 + LIMIT_MS - 1, false, true), Beat::Idle);
        assert_eq!(beat.poll(1_000 + LIMIT_MS, false, true), Beat::Hung);
        // Still hung while it stays unanswered.
        assert_eq!(beat.poll(1_000 + LIMIT_MS + 5, false, true), Beat::Hung);
    }

    #[test]
    fn a_late_answer_clears_the_alarm() {
        let mut beat = Heartbeat::new();
        assert_eq!(beat.poll(0, true, true), Beat::Send);
        assert_eq!(beat.poll(LIMIT_MS - 1, false, true), Beat::Idle);
        assert_eq!(beat.poll(LIMIT_MS + 3_000, true, true), Beat::Idle);
    }

    #[test]
    fn the_last_view_going_does_not_leave_a_probe_to_expire() {
        let mut beat = Heartbeat::new();
        assert_eq!(beat.poll(0, true, true), Beat::Send);
        // The view is gone but the probe is still out: it is still judged (the loop is what hangs).
        assert_eq!(beat.poll(LIMIT_MS, false, false), Beat::Hung);
    }
}
