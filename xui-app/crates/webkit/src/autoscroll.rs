//! Autoscroll: a middle click on bare page starts it, and the page then scrolls on its own, faster
//! the further the pointer is from where the click was (above it up, below it down). A second click,
//! Escape or the wheel ends it; so does letting go of the button after dragging away from the
//! start (the button held down, as on Windows).
//!
//! This is only the arithmetic. `view_input.rs` decides when it starts and stops, and turns the
//! ticks it gives into wheel notches, so the picture glides as it does for a real wheel.

use crate::speculate::{LINE_PX, MAX_TICKS};

/// How far (pixels) the pointer may be from the start before the page moves, and before a release
/// of the button counts as the end of a drag.
pub(crate) const DEAD_PX: i32 = 12;
/// Pixels per second of page speed for each pixel of distance beyond the dead zone.
const GAIN: f32 = 8.0;
/// The longest time one step may account for (a stalled window must not jump the page).
const STEP_MAX_US: u64 = 100_000;

#[derive(Debug, Default)]
pub(crate) struct AutoScroll {
    active: bool,
    anchor: (i32, i32),
    at: (i32, i32),
    /// The pointer has been past the dead zone since the start.
    dragged: bool,
    /// Notches owed from steps too short to make a whole one (positive: down).
    carry: f32,
    last_us: u64,
}

impl AutoScroll {
    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn start(&mut self, x: i32, y: i32, now_us: u64) {
        *self = AutoScroll {
            active: true,
            anchor: (x, y),
            at: (x, y),
            dragged: false,
            carry: 0.0,
            last_us: now_us,
        };
    }

    pub(crate) fn stop(&mut self) {
        self.active = false;
    }

    /// The pointer is at (`x`, `y`).
    pub(crate) fn moved(&mut self, x: i32, y: i32) {
        self.at = (x, y);
        self.dragged |= self.offset().abs() > DEAD_PX;
    }

    /// The pointer's position, for the wheel events the steps become.
    pub(crate) fn pointer(&self) -> (i32, i32) {
        self.at
    }

    /// Whether letting go of the button now ends it (the pointer was dragged away).
    pub(crate) fn release_ends(&self) -> bool {
        self.dragged
    }

    /// Pixels below (positive) or above the start, past nothing.
    fn offset(&self) -> i32 {
        self.at.1.saturating_sub(self.anchor.1)
    }

    /// The whole notches to scroll at `now_us` (positive: the page moves down), keeping the rest
    /// for the next step.
    pub(crate) fn step(&mut self, now_us: u64) -> i32 {
        let dt = now_us.saturating_sub(self.last_us).min(STEP_MAX_US);
        self.last_us = now_us;
        let offset = self.offset();
        let beyond = offset.abs().saturating_sub(DEAD_PX);
        if !self.active || beyond == 0 {
            self.carry = 0.0;
            return 0;
        }
        let pixels = beyond as f32 * GAIN * dt as f32 / 1e6;
        self.carry += offset.signum() as f32 * pixels / LINE_PX as f32;
        let whole = (self.carry.trunc() as i32).clamp(-MAX_TICKS, MAX_TICKS);
        self.carry -= whole as f32;
        whole
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000;

    fn started() -> AutoScroll {
        let mut auto = AutoScroll::default();
        auto.start(100, 200, 0);
        auto
    }

    #[test]
    fn inside_the_dead_zone_nothing_moves() {
        let mut auto = started();
        auto.moved(100, 200 + DEAD_PX);
        assert_eq!(auto.step(16 * MS), 0);
        assert!(!auto.release_ends(), "not dragged away");
    }

    #[test]
    fn below_the_start_scrolls_down_and_above_it_up() {
        let mut auto = started();
        auto.moved(100, 200 + 150);
        let down: i32 = (1..=60).map(|n| auto.step(n * 16 * MS)).sum();
        assert!(down > 0, "{down}");
        let mut auto = started();
        auto.moved(100, 200 - 150);
        let up: i32 = (1..=60).map(|n| auto.step(n * 16 * MS)).sum();
        assert!(up < 0, "{up}");
        assert_eq!(down, -up, "same distance, same speed");
    }

    #[test]
    fn the_further_the_pointer_the_faster_the_page() {
        let total = |dy: i32| {
            let mut auto = started();
            auto.moved(100, 200 + dy);
            (1..=60).map(|n| auto.step(n * 16 * MS)).sum::<i32>()
        };
        assert!(total(60) < total(120) && total(120) < total(240));
    }

    #[test]
    fn short_steps_add_up_to_whole_notches() {
        let mut auto = started();
        auto.moved(100, 200 + 60);
        // 48 px beyond the dead zone: 384 px/s, about 10 notches a second.
        let notches: i32 = (1..=100).map(|n| auto.step(n * 10 * MS)).sum();
        assert!((8..=11).contains(&notches), "{notches}");
    }

    #[test]
    fn a_stalled_window_does_not_jump_the_page() {
        let mut auto = started();
        auto.moved(100, 200 + 150);
        assert!(auto.step(60_000 * MS) <= 2, "one step is capped at 100 ms");
    }

    #[test]
    fn a_far_pointer_is_capped_per_step() {
        let mut auto = started();
        auto.moved(100, 200 + 100_000);
        assert!(auto.step(100 * MS) <= MAX_TICKS);
    }

    #[test]
    fn dragging_away_makes_the_release_the_end() {
        let mut auto = started();
        auto.moved(100, 200 + DEAD_PX + 1);
        assert!(auto.release_ends());
    }

    #[test]
    fn stopped_scrolls_nothing() {
        let mut auto = started();
        auto.moved(100, 400);
        auto.stop();
        assert!(!auto.is_active());
        assert_eq!(auto.step(100 * MS), 0);
    }
}
