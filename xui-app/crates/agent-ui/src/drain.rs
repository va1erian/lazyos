//! The UI tick's drain of the core's events (docs/ai-agent-plan.md §2.1).

use std::time::{Duration, Instant};

use lazyagent_core::{Event, Events};

/// The most one tick spends taking events.
pub const BUDGET: Duration = Duration::from_millis(4);

/// The timer interval while anything is pending.
pub const BUSY_MS: u32 = 16;
/// The timer interval when idle.
pub const IDLE_MS: u32 = 250;

/// What one tick took.
pub struct Drained {
    pub events: Vec<Event>,
    /// The budget ran out with events possibly still waiting.
    pub cut_short: bool,
}

/// Takes events until none wait or `budget` is spent. At least one event is
/// taken per call when one is waiting, so a zero budget still makes
/// progress; what is left waits for the next tick.
pub fn drain(events: &Events, budget: Duration) -> Drained {
    let started = Instant::now();
    let mut taken = Vec::new();
    let mut cut_short = false;
    while let Some(event) = events.try_next() {
        taken.push(event);
        if started.elapsed() >= budget {
            cut_short = true;
            break;
        }
    }
    Drained {
        events: taken,
        cut_short,
    }
}

/// How long the timer should wait before the next tick.
pub fn interval(busy: bool, pending: usize, more_waiting: bool, retry: bool) -> u32 {
    if busy || pending > 0 || more_waiting || retry {
        BUSY_MS
    } else {
        IDLE_MS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_timer_is_fast_only_while_work_is_pending() {
        assert_eq!(interval(false, 0, false, false), IDLE_MS);
        assert_eq!(interval(true, 0, false, false), BUSY_MS);
        assert_eq!(interval(false, 2, false, false), BUSY_MS);
        assert_eq!(interval(false, 0, true, false), BUSY_MS);
        assert_eq!(interval(false, 0, false, true), BUSY_MS);
    }
}
