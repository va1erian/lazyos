//! When to ask next. Pure: every method takes the monotonic clock in
//! milliseconds and, where it needs one, a random `jitter` from the caller,
//! so the same sequence of calls always gives the same schedule.
//!
//! * No query while there is no network; an address appearing makes the
//!   first one due at once (and later ones when the last success is old).
//! * A failure backs off 5 s, 10 s, 20 s, ... up to 5 minutes; a success
//!   waits an hour. Every wait is spread by +-12.5 %.
//! * A `RATE` kiss waits 15 minutes, then four times as long, up to 4 hours.
//! * No two queries are closer than [`MIN_INTERVAL_MS`], whatever asked
//!   (RFC 4330 section 10 says 15 s at the very least), including a user's
//!   "sync now".

/// The least time between two queries.
pub const MIN_INTERVAL_MS: u64 = 15_000;
/// The first retry delay; each failure doubles it.
pub const RETRY_FIRST_MS: u64 = 5_000;
/// The longest retry delay.
pub const RETRY_MAX_MS: u64 = 300_000;
/// The wait after a success.
pub const POLL_MS: u64 = 3_600_000;
/// The first wait after a `RATE` kiss.
pub const RATE_FIRST_MS: u64 = 900_000;
/// The longest wait after repeated `RATE` kisses.
pub const RATE_MAX_MS: u64 = 14_400_000;
/// A network that comes back syncs again at once if the last success is
/// older than this.
pub const RESYNC_AFTER_UP_MS: u64 = 600_000;

/// `base` spread by -12.5 % to +12.5 % according to `jitter`.
pub fn spread(base: u64, jitter: u32) -> u64 {
    let permille = i128::from(jitter % 251) - 125;
    let delta = i128::from(base) * permille / 1000;
    (i128::from(base) + delta).max(0) as u64
}

#[derive(Clone, Debug)]
pub struct Schedule {
    network: bool,
    next_ms: u64,
    last_attempt_ms: Option<u64>,
    last_success_ms: Option<u64>,
    failures: u32,
    rate_level: u32,
    /// No query before this (a server asked to be left alone for a while):
    /// neither a user's "sync now" nor a network coming back lifts it.
    hold_until_ms: u64,
    synced_once: bool,
}

impl Default for Schedule {
    fn default() -> Schedule {
        Schedule::new()
    }
}

impl Schedule {
    pub const fn new() -> Schedule {
        Schedule {
            network: false,
            next_ms: 0,
            last_attempt_ms: None,
            last_success_ms: None,
            failures: 0,
            rate_level: 0,
            hold_until_ms: 0,
            synced_once: false,
        }
    }

    /// The network came up (`true`) or went away. Idempotent: only an edge
    /// changes anything, so a poll may report the same state every time.
    pub fn set_network(&mut self, up: bool, now_ms: u64) {
        if up == self.network {
            return;
        }
        self.network = up;
        if !up {
            return;
        }
        // A different network: its first failures start the back-off over.
        self.failures = 0;
        let stale = self
            .last_success_ms
            .is_none_or(|at| now_ms.saturating_sub(at) >= RESYNC_AFTER_UP_MS);
        if stale {
            self.next_ms = now_ms;
        }
    }

    pub fn network(&self) -> bool {
        self.network
    }

    /// The monotonic millisecond the next query may go out, `None` while
    /// there is no network.
    pub fn due_at(&self) -> Option<u64> {
        if !self.network {
            return None;
        }
        let floor = self
            .last_attempt_ms
            .map_or(0, |at| at.saturating_add(MIN_INTERVAL_MS));
        Some(self.next_ms.max(floor).max(self.hold_until_ms))
    }

    pub fn is_due(&self, now_ms: u64) -> bool {
        self.due_at().is_some_and(|at| now_ms >= at)
    }

    /// Record that a query went out now.
    pub fn attempted(&mut self, now_ms: u64) {
        self.last_attempt_ms = Some(now_ms);
    }

    pub fn succeeded(&mut self, now_ms: u64, jitter: u32) {
        self.failures = 0;
        self.rate_level = 0;
        self.hold_until_ms = 0;
        self.synced_once = true;
        self.last_success_ms = Some(now_ms);
        self.next_ms = now_ms.saturating_add(spread(POLL_MS, jitter));
    }

    pub fn failed(&mut self, now_ms: u64, jitter: u32) {
        self.failures = self.failures.saturating_add(1);
        let doublings = (self.failures - 1).min(16);
        let wait = (RETRY_FIRST_MS << doublings).min(RETRY_MAX_MS);
        self.next_ms = now_ms.saturating_add(spread(wait, jitter));
    }

    /// A `RATE` kiss: much longer than any failure.
    pub fn rate_limited(&mut self, now_ms: u64, jitter: u32) {
        self.rate_level = self.rate_level.saturating_add(1);
        let quadruplings = (self.rate_level - 1).min(8) * 2;
        let wait = (RATE_FIRST_MS << quadruplings).min(RATE_MAX_MS);
        self.next_ms = now_ms.saturating_add(spread(wait, jitter));
        self.hold_until_ms = self.next_ms;
    }

    /// A request to sync now (the user's button). Honoured only with a
    /// network, past the minimum interval and outside a `RATE` hold; returns
    /// whether it was.
    pub fn sync_now(&mut self, now_ms: u64) -> bool {
        if !self.network || now_ms < self.hold_until_ms {
            return false;
        }
        let recent = self
            .last_attempt_ms
            .is_some_and(|at| now_ms.saturating_sub(at) < MIN_INTERVAL_MS);
        if recent {
            return false;
        }
        self.next_ms = now_ms;
        true
    }

    /// Whether no exchange has succeeded since boot yet (the first one may
    /// take a large step, `policy::decide`).
    pub fn first_sync(&self) -> bool {
        !self.synced_once
    }

    pub fn last_success_ms(&self) -> Option<u64> {
        self.last_success_ms
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `jitter % 251 == 125` adds nothing, so waits read exactly.
    const EXACT: u32 = 125;

    #[test]
    fn nothing_is_due_without_a_network() {
        let s = Schedule::new();
        assert_eq!(s.due_at(), None);
        assert!(!s.is_due(u64::MAX));
        assert!(s.first_sync());
    }

    #[test]
    fn an_address_makes_the_first_query_due_at_once() {
        let mut s = Schedule::new();
        s.set_network(true, 7_000);
        assert_eq!(s.due_at(), Some(7_000));
        assert!(s.is_due(7_000));
        assert!(!s.is_due(6_999));
    }

    #[test]
    fn failures_back_off_and_cap() {
        let mut s = Schedule::new();
        s.set_network(true, 0);
        let mut now = 0;
        let mut waits = Vec::new();
        for _ in 0..9 {
            s.attempted(now);
            s.failed(now, EXACT);
            let due = s.due_at().unwrap();
            waits.push(due - now);
            now = due;
        }
        // The first two waits are lifted to the 15 s minimum interval.
        assert_eq!(
            waits,
            [15_000, 15_000, 20_000, 40_000, 80_000, 160_000, 300_000, 300_000, 300_000]
        );
    }

    #[test]
    fn success_waits_an_hour_and_forgives_failures() {
        let mut s = Schedule::new();
        s.set_network(true, 0);
        s.attempted(0);
        s.failed(0, EXACT);
        s.failed(5_000, EXACT);
        assert_eq!(s.failures(), 2);
        s.attempted(20_000);
        s.succeeded(20_100, EXACT);
        assert_eq!(s.failures(), 0);
        assert!(!s.first_sync());
        assert_eq!(s.due_at(), Some(20_100 + POLL_MS));
        assert_eq!(s.last_success_ms(), Some(20_100));
    }

    #[test]
    fn rate_kisses_back_off_by_fours() {
        let mut s = Schedule::new();
        s.set_network(true, 0);
        s.rate_limited(0, EXACT);
        assert_eq!(s.due_at(), Some(900_000));
        s.rate_limited(0, EXACT);
        assert_eq!(s.due_at(), Some(3_600_000));
        s.rate_limited(0, EXACT);
        assert_eq!(s.due_at(), Some(RATE_MAX_MS));
        for _ in 0..100 {
            s.rate_limited(0, EXACT);
        }
        assert_eq!(s.due_at(), Some(RATE_MAX_MS));
        s.succeeded(1, EXACT);
        s.rate_limited(1, EXACT);
        assert_eq!(s.due_at(), Some(1 + 900_000));
    }

    #[test]
    fn a_rate_hold_outlives_sync_now_and_a_network_edge() {
        let mut s = Schedule::new();
        s.set_network(true, 0);
        s.attempted(0);
        s.rate_limited(0, EXACT);
        let hold = s.due_at().unwrap();
        assert_eq!(hold, RATE_FIRST_MS);
        assert!(
            !s.sync_now(60_000),
            "the user's button is not a way round the kiss"
        );
        assert_eq!(s.due_at(), Some(hold));
        s.set_network(false, 70_000);
        s.set_network(true, 80_000);
        assert_eq!(
            s.due_at(),
            Some(hold),
            "a link flap does not lift it either"
        );
        assert!(!s.is_due(hold - 1));
        assert!(s.is_due(hold));
        assert!(s.sync_now(hold));
    }

    #[test]
    fn no_two_queries_are_closer_than_the_minimum() {
        let mut s = Schedule::new();
        s.set_network(true, 0);
        s.attempted(1_000);
        s.next_ms = 0; // whatever the policy said
        assert_eq!(s.due_at(), Some(1_000 + MIN_INTERVAL_MS));
        assert!(!s.sync_now(1_000 + MIN_INTERVAL_MS - 1));
        assert!(s.sync_now(1_000 + MIN_INTERVAL_MS));
        assert!(s.is_due(1_000 + MIN_INTERVAL_MS));
    }

    #[test]
    fn sync_now_needs_a_network() {
        let mut s = Schedule::new();
        assert!(!s.sync_now(1_000_000));
    }

    #[test]
    fn network_edges_are_idempotent() {
        let mut s = Schedule::new();
        s.set_network(true, 0);
        s.attempted(0);
        s.failed(0, EXACT);
        let due = s.due_at();
        // Reporting "up" again (a poll) changes nothing.
        s.set_network(true, 1_000);
        assert_eq!(s.due_at(), due);
        assert_eq!(s.failures(), 1);
    }

    #[test]
    fn a_new_network_starts_over_and_a_recent_success_is_kept() {
        let mut s = Schedule::new();
        s.set_network(true, 0);
        s.attempted(0);
        s.succeeded(100, EXACT);
        // Link flaps at once: the clock was just set, the hour stands.
        s.set_network(false, 200);
        assert_eq!(s.due_at(), None);
        s.set_network(true, 300);
        assert_eq!(s.due_at(), Some(100 + POLL_MS));
        // A new network after a long while syncs at once.
        s.set_network(false, 400);
        let later = 100 + RESYNC_AFTER_UP_MS;
        s.set_network(true, later);
        assert_eq!(s.due_at(), Some(later.max(15_000)));
    }

    #[test]
    fn spread_stays_within_an_eighth() {
        for base in [5_000u64, 300_000, 3_600_000, 14_400_000] {
            for jitter in 0..1_000u32 {
                let v = spread(base, jitter);
                assert!(
                    v >= base - base / 8 && v <= base + base / 8,
                    "{base} {jitter} {v}"
                );
            }
        }
        assert_eq!(spread(1_000, 125), 1_000);
        assert_eq!(spread(0, 7), 0);
        assert_eq!(spread(u64::MAX, u32::MAX), spread(u64::MAX, u32::MAX));
    }

    #[test]
    fn a_long_run_never_overflows() {
        let mut s = Schedule::new();
        s.set_network(true, u64::MAX - 10);
        for i in 0..1_000u64 {
            s.attempted(u64::MAX - 5);
            s.failed(u64::MAX - 5, i as u32);
            s.rate_limited(u64::MAX - 5, i as u32);
            s.succeeded(u64::MAX - 5, i as u32);
            let _ = s.sync_now(u64::MAX);
        }
    }
}
