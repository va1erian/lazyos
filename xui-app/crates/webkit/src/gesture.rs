//! The last real input the user gave the page: a second lock behind the engine's user-gesture flag.
//!
//! WebKit says whether a navigation began with a user gesture (`WKNavigationActionHasUnconsumedUserGesture`).
//! That is one boolean from a large engine, and a click is allowed to authorise more than one thing
//! (a timer chain, a redirect). Starting another program is the one navigation that must never happen
//! without the person, so the window also records when it last *forwarded* a mouse button or a key to
//! the page, and a hand-off needs one of those within a short time, used once.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Milliseconds since the first call, plus one (zero means "no input").
static LAST_INPUT: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

/// The window forwarded a mouse button or a key to the page.
pub fn stamp() {
    LAST_INPUT.store(now_ms(), Ordering::Relaxed);
}

/// Whether the page got real input within `within`, without using it up: for a decision that only
/// asks the person something (a site's request to notify them), which one click may start with
/// others.
pub fn recent(within: Duration) -> bool {
    let at = LAST_INPUT.load(Ordering::Relaxed);
    at != 0 && now_ms().saturating_sub(at) <= within.as_millis() as u64
}

/// Whether the page got real input within `within`, and if so the input is used up: one input
/// authorises one hand-off.
pub fn take_recent(within: Duration) -> bool {
    let at = LAST_INPUT.swap(0, Ordering::Relaxed);
    at != 0 && now_ms().saturating_sub(at) <= within.as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The stamp is one process-wide value: the tests take turns with it.
    static TURN: Mutex<()> = Mutex::new(());

    fn turn() -> std::sync::MutexGuard<'static, ()> {
        TURN.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn input_authorises_one_hand_off() {
        let _turn = turn();
        stamp();
        assert!(take_recent(Duration::from_secs(5)));
        assert!(!take_recent(Duration::from_secs(5)), "used up");
    }

    #[test]
    fn looking_at_the_input_does_not_use_it_up() {
        let _turn = turn();
        stamp();
        assert!(recent(Duration::from_secs(5)));
        assert!(recent(Duration::from_secs(5)));
        assert!(take_recent(Duration::from_secs(5)));
        assert!(!recent(Duration::from_secs(5)));
    }

    #[test]
    fn no_input_authorises_nothing() {
        let _turn = turn();
        let _ = take_recent(Duration::from_secs(5));
        assert!(!take_recent(Duration::from_secs(5)));
    }

    #[test]
    fn old_input_does_not_count() {
        let _turn = turn();
        stamp();
        std::thread::sleep(Duration::from_millis(30));
        assert!(!take_recent(Duration::from_millis(5)));
    }
}
