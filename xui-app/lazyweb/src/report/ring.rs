//! What a report may quote from this run, kept in memory and bounded: the window's own `LazyWeb`
//! trace (every line [`crate::marker`] prints goes through [`record`]) and the engine events the
//! window saw (a lost view with its reason, a page that stopped answering, what the person answered).
//!
//! Neither holds page content: the trace lines are the window's evidence markers (the ones that
//! name an address are printed only in a UI-probe image), the events are fixed words and times.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::sanitize;

/// How many trace lines are kept.
pub const RING_LINES: usize = 200;
/// The longest trace line kept, in characters.
pub const LINE_MAX: usize = 300;
/// How many engine events are kept.
pub const EVENTS_MAX: usize = 64;
/// The longest engine event kept, in characters.
const EVENT_MAX: usize = 200;

static TRACE: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// Keeps one trace line (without its `LAZYWEB:` prefix), dropping the oldest when the ring is full.
pub fn record(line: &str) {
    // A panic elsewhere must not stop the trace: the data is plain text and stays whole.
    let mut ring = TRACE.lock().unwrap_or_else(|e| e.into_inner());
    if ring.len() == RING_LINES {
        ring.pop_front();
    }
    ring.push_back(entry(line));
}

/// The ring's form of a trace line: the serial line it was, on one line and bounded.
fn entry(line: &str) -> String {
    format!("LAZYWEB:{}", sanitize::line(line, LINE_MAX))
}

/// The newest `n` lines, oldest first.
pub fn recent(n: usize) -> Vec<String> {
    let ring = TRACE.lock().unwrap_or_else(|e| e.into_inner());
    let skip = ring.len().saturating_sub(n);
    ring.iter().skip(skip).cloned().collect()
}

/// The engine events this run saw, newest last.
#[derive(Debug, Default)]
pub struct EngineLog {
    events: VecDeque<String>,
}

impl EngineLog {
    /// Notes that `what` happened `seconds` into the run.
    pub fn note(&mut self, seconds: u64, what: &str) {
        if self.events.len() == EVENTS_MAX {
            self.events.pop_front();
        }
        self.events
            .push_back(sanitize::line(&format!("+{seconds}s: {what}"), EVENT_MAX));
    }

    pub fn lines(&self) -> Vec<String> {
        self.events.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_keeps_the_newest_lines_and_cleans_them() {
        // The ring is global and other tests print markers: look only at lines made here.
        for n in 0..RING_LINES + 50 {
            record(&format!("RING:TEST:{n}\u{202E}\nx"));
        }
        let all = recent(RING_LINES);
        assert_eq!(all.len(), RING_LINES);
        assert!(all.iter().all(|l| l.starts_with("LAZYWEB:")));
        assert!(all
            .iter()
            .all(|l| !l.contains('\n') && !l.contains('\u{202E}')));
        assert!(recent(3).len() == 3);
        assert_eq!(recent(1000).len(), RING_LINES);
    }

    #[test]
    fn a_long_line_is_cut() {
        // (Not through the global ring: other tests print markers at the same time.)
        let cut = entry(&"x".repeat(5_000));
        assert_eq!(cut.chars().count(), "LAZYWEB:".len() + LINE_MAX);
    }

    #[test]
    fn the_event_list_is_bounded_and_timed() {
        let mut log = EngineLog::default();
        for n in 0..EVENTS_MAX + 10 {
            log.note(n as u64, "page not responding");
        }
        let lines = log.lines();
        assert_eq!(lines.len(), EVENTS_MAX);
        assert_eq!(lines[0], "+10s: page not responding");
        assert_eq!(lines.last().unwrap(), "+73s: page not responding");
        log.note(1, "a\nb\u{202E}c");
        assert_eq!(log.lines().last().unwrap(), "+1s: abc");
    }
}
