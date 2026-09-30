//! Key repeat. The kernel bus carries only press/release edges, so the repeat
//! that hardware typematic used to provide lives here, with fixed constants
//! (`confd`-configurable timing is a later enrichment).

/// Nanoseconds per PIT tick (100 Hz), the unit callers pass time in.
pub const TICK_NS: u64 = 10_000_000;

/// Held this long before the first repeat: 500 ms.
pub const REPEAT_DELAY_TICKS: u64 = 50;

/// Between repeats: 30 ms (about 33 per second).
pub const REPEAT_INTERVAL_TICKS: u64 = 3;

/// The one key currently repeating, if any. Like every keyboard, only the
/// most recently pressed repeatable key repeats.
#[derive(Default)]
pub(crate) struct Repeater {
    active: Option<(u16, u64)>,
}

impl Repeater {
    /// Start repeating `code` after the initial delay (replaces any other key).
    pub(crate) fn start(&mut self, code: u16, now: u64) {
        self.active = Some((code, now + REPEAT_DELAY_TICKS));
    }

    /// Stop repeating `code` (a release of some other key changes nothing).
    pub(crate) fn release(&mut self, code: u16) {
        if matches!(self.active, Some((held, _)) if held == code) {
            self.active = None;
        }
    }

    /// Stop repeating anything (focus change, resync).
    pub(crate) fn cancel(&mut self) {
        self.active = None;
    }

    /// The key due for a repeat at `now`, if any; schedules the next one. A
    /// late poll yields one repeat, not a burst to catch up.
    pub(crate) fn poll(&mut self, now: u64) -> Option<u16> {
        let (code, due) = self.active?;
        if now < due {
            return None;
        }
        self.active = Some((code, now + REPEAT_INTERVAL_TICKS));
        Some(code)
    }

    /// The tick the next repeat is due at, so the service can sleep exactly
    /// until then.
    pub(crate) fn next_due(&self) -> Option<u64> {
        self.active.map(|(_, due)| due)
    }
}
