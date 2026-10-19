//! Frame pacing (client mode): a window presents at most once per refresh of the screen.
//!
//! A frame composed between two refreshes is never seen, and its cost lands on the next one: a
//! scrolling LazyWeb+ window presented about a hundred whole frames a second at 2560x1440 (its glide
//! timer and the engine's updates, each drawing at once), so `xuid` composed and copied 1.8 megapixels
//! a hundred times a second for a 60 Hz screen (`XUID:COMPOSE`). Damage that arrives sooner than one
//! refresh interval after the last present waits in the damage map (it accumulates there) and the loop
//! parks until the interval is over, so the next present carries all of it.
//!
//! The interval is the compositor's `GetOutput.refresh_millihz` (the rate `gpud` measured on the Kaby
//! Lake box, 60 Hz otherwise). Presents keep their cadence: a present made a little after its slot
//! counts from the slot, not from when it ran, so a late wake does not push every later frame back;
//! one made after a pause starts a new cadence.

use std::collections::HashMap;

/// 60 Hz, when the compositor names no rate.
pub const DEFAULT_INTERVAL_NS: u64 = 16_666_667;
/// Intervals outside these bounds (250 Hz to 20 Hz) are not a screen's: the default is used.
const MIN_INTERVAL_NS: u64 = 4_000_000;
const MAX_INTERVAL_NS: u64 = 50_000_000;

/// The refresh interval for a rate in millihertz (`GetOutput.refresh_millihz`).
pub fn interval_for(millihz: u32) -> u64 {
    if millihz == 0 {
        return DEFAULT_INTERVAL_NS;
    }
    let interval = 1_000_000_000_000 / u64::from(millihz);
    if (MIN_INTERVAL_NS..=MAX_INTERVAL_NS).contains(&interval) {
        interval
    } else {
        DEFAULT_INTERVAL_NS
    }
}

/// When each window last presented, against one interval.
#[derive(Debug)]
pub struct Pacer {
    interval_ns: u64,
    /// By window: the slot the last present counts from.
    last: HashMap<u64, u64>,
}

impl Pacer {
    pub fn new(interval_ns: u64) -> Pacer {
        Pacer {
            interval_ns,
            last: HashMap::new(),
        }
    }

    /// `None` when `window` may present at `now`, else the time it may.
    pub fn wait(&self, window: u64, now: u64) -> Option<u64> {
        let next = self.last.get(&window)?.saturating_add(self.interval_ns);
        (now < next).then_some(next)
    }

    /// `window` presented at `now`.
    pub fn presented(&mut self, window: u64, now: u64) {
        let slot = match self.last.get(&window) {
            // On time or a little late (within one interval of its slot): the cadence holds.
            Some(&last) => {
                let next = last.saturating_add(self.interval_ns);
                if now >= next && now - next < self.interval_ns {
                    next
                } else {
                    now
                }
            }
            None => now,
        };
        self.last.insert(window, slot);
    }

    /// `window` is gone.
    pub fn forget(&mut self, window: u64) {
        self.last.remove(&window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    #[test]
    fn the_first_present_and_one_after_a_pause_go_at_once() {
        let mut pacer = Pacer::new(16 * MS);
        assert_eq!(pacer.wait(1, 1000 * MS), None);
        pacer.presented(1, 1000 * MS);
        assert_eq!(pacer.wait(1, 2000 * MS), None, "long after: at once");
        assert_eq!(pacer.wait(2, 1000 * MS), None, "windows are paced apart");
    }

    #[test]
    fn damage_sooner_than_an_interval_waits_for_the_next_slot() {
        let mut pacer = Pacer::new(16 * MS);
        pacer.presented(1, 100 * MS);
        assert_eq!(pacer.wait(1, 101 * MS), Some(116 * MS));
        assert_eq!(pacer.wait(1, 115 * MS), Some(116 * MS));
        assert_eq!(pacer.wait(1, 116 * MS), None);
    }

    #[test]
    fn a_late_present_keeps_the_cadence_and_a_pause_starts_a_new_one() {
        let mut pacer = Pacer::new(16 * MS);
        pacer.presented(1, 100 * MS);
        pacer.presented(1, 119 * MS); // 3 ms late for its 116 slot
        assert_eq!(pacer.wait(1, 120 * MS), Some(132 * MS));
        pacer.presented(1, 500 * MS); // after a pause
        assert_eq!(pacer.wait(1, 501 * MS), Some(516 * MS));
    }

    #[test]
    fn a_hundred_frames_a_second_become_sixty() {
        let mut pacer = Pacer::new(interval_for(60_000));
        let mut shown = 0;
        // Damage every 10 ms for a second; the loop presents when the pacer lets it, or wakes at the
        // slot it names.
        let mut now = 0;
        let mut pending = false;
        let mut wake = u64::MAX;
        while now < 1000 * MS {
            if now % (10 * MS) == 0 {
                pending = true;
            }
            if pending && (now >= wake || pacer.wait(1, now).is_none()) {
                pacer.presented(1, now);
                shown += 1;
                pending = false;
                wake = u64::MAX;
            } else if pending {
                wake = pacer.wait(1, now).unwrap_or(now);
            }
            now += MS / 4;
        }
        assert!((59..=61).contains(&shown), "{shown} presents");
    }

    #[test]
    fn rates_become_intervals_within_bounds() {
        assert_eq!(interval_for(60_000), 16_666_666);
        assert_eq!(interval_for(59_950), 16_680_567);
        assert_eq!(interval_for(0), DEFAULT_INTERVAL_NS);
        assert_eq!(
            interval_for(1_000_000),
            DEFAULT_INTERVAL_NS,
            "1 kHz is no screen"
        );
        assert_eq!(
            interval_for(5_000),
            DEFAULT_INTERVAL_NS,
            "5 Hz is no screen"
        );
        let mut pacer = Pacer::new(16 * MS);
        pacer.presented(3, 0);
        pacer.forget(3);
        assert_eq!(pacer.wait(3, 1), None);
    }
}
