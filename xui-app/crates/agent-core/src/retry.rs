//! Retry timing (docs/ai-agent-plan.md §4, Errors): 429 and 5xx are retried
//! at most twice, after the server's `Retry-After` when it gave one, with an
//! exponential backoff otherwise. Waiting can be cut short by Stop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retry {
    /// Retries after the first attempt.
    pub max: u32,
    /// The wait before the first retry; it doubles each time.
    pub base: Duration,
    /// No wait is longer than this, whatever the server asks.
    pub cap: Duration,
}

impl Default for Retry {
    fn default() -> Retry {
        Retry {
            max: 2,
            base: Duration::from_secs(1),
            cap: Duration::from_secs(30),
        }
    }
}

impl Retry {
    /// The wait before retry number `attempt` (0 for the first retry).
    pub fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        let backoff = self.base.saturating_mul(1u32 << attempt.min(16));
        retry_after.unwrap_or(backoff).min(self.cap)
    }
}

/// A `Retry-After` header: whole seconds. (An HTTP date is not worth a date
/// parser here; the backoff covers it.)
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let seconds: f64 = value.trim().parse().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds.min(86_400.0)))
}

/// Sleeps `total` in short steps; `false` when `stop` was set meanwhile.
pub fn sleep_unless_stopped(total: Duration, stop: &AtomicBool) -> bool {
    let end = Instant::now() + total;
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        std::thread::sleep(left.min(Duration::from_millis(50)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_wins_over_the_backoff_but_not_over_the_cap() {
        let retry = Retry::default();
        assert_eq!(retry.delay(0, None), Duration::from_secs(1));
        assert_eq!(retry.delay(1, None), Duration::from_secs(2));
        assert_eq!(
            retry.delay(0, Some(Duration::from_secs(7))),
            Duration::from_secs(7)
        );
        assert_eq!(
            retry.delay(0, Some(Duration::from_secs(3600))),
            Duration::from_secs(30)
        );
        assert_eq!(retry.delay(40, None), Duration::from_secs(30));
    }

    #[test]
    fn retry_after_is_seconds_or_nothing() {
        assert_eq!(parse_retry_after("3"), Some(Duration::from_secs(3)));
        assert_eq!(parse_retry_after(" 0.5 "), Some(Duration::from_millis(500)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("-1"), None);
        assert_eq!(parse_retry_after("NaN"), None);
    }

    #[test]
    fn a_stop_cuts_a_wait_short() {
        let stop = AtomicBool::new(true);
        let started = Instant::now();
        assert!(!sleep_unless_stopped(Duration::from_secs(5), &stop));
        assert!(started.elapsed() < Duration::from_millis(200));
        assert!(sleep_unless_stopped(
            Duration::from_millis(10),
            &AtomicBool::new(false)
        ));
    }
}
