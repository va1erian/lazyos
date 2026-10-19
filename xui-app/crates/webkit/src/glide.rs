//! Smooth scrolling: the picture glides to where the wheel asked instead of jumping a notch at a time.
//!
//! `speculate.rs` says where the page should be (the page's offset plus what the wheel asked and the page
//! has not done yet, the *target*). A notch moves the target 40 px at once; drawn as it is, a wheel turned
//! at 15 notches a second is 15 jumps a second. The window instead keeps the page row shown at the top of
//! the view (`shown`) and moves it toward the target on a frame timer, easing out (an exponential with
//! [`TAU_MS`]), so a notch is a short slide over several frames.
//!
//! The target also moves when the page does something nobody asked (a script scrolls, an anchor, a new
//! page): that is followed at once, not glided ([`Glide::follow`]). Only the wheel (and a scroll ahead
//! given up on, which glides back) is animated. Numbers only, tested on the host.

use std::cell::Cell;

thread_local! {
    /// Whether the wheel glides (on by default); off, the picture jumps to the target as it used to.
    static SMOOTH: Cell<bool> = const { Cell::new(true) };
}

/// Turns smooth scrolling on or off for the views of this thread (a preference; tests that check where a
/// notch lands turn it off).
pub fn set_smooth(on: bool) {
    SMOOTH.with(|smooth| smooth.set(on));
}

fn smooth() -> bool {
    SMOOTH.with(Cell::get)
}

/// The time constant of the ease-out: 95% of a step is done after three of these.
pub const TAU_MS: f64 = 45.0;
/// Closer than this to the target (page pixels) is there.
const ARRIVED: f64 = 0.5;
/// A frame later than this is drawn as if it were this late (a stalled window does not leap).
const MAX_STEP_MS: f64 = 50.0;

#[derive(Debug, Default)]
pub struct Glide {
    /// The page row shown at the top of the view; `None` until there is a target.
    shown: Option<f64>,
    last_us: Option<u64>,
}

impl Glide {
    /// The page row to draw at the top of the view for `target` (the page row the wheel asked for).
    pub fn top(&self, target: i32) -> i32 {
        match self.shown {
            Some(shown) if smooth() => shown.round() as i32,
            _ => target,
        }
    }

    /// Whether the picture is still on its way to `target`.
    pub fn moving(&self, target: i32) -> bool {
        smooth()
            && self
                .shown
                .is_some_and(|shown| (shown - f64::from(target)).abs() >= ARRIVED)
    }

    /// One frame at `now_us`: moves toward `target`. Returns whether the shown row changed.
    pub fn step(&mut self, target: i32, now_us: u64) -> bool {
        let target = f64::from(target);
        let Some(shown) = self.shown else {
            self.shown = Some(target);
            self.last_us = Some(now_us);
            return false;
        };
        let elapsed_ms = self
            .last_us
            .map_or(0.0, |last| now_us.saturating_sub(last) as f64 / 1000.0)
            .min(MAX_STEP_MS);
        self.last_us = Some(now_us);
        let before = shown.round();
        let gap = target - shown;
        let next = if gap.abs() < ARRIVED {
            target
        } else {
            let next = shown + gap * (1.0 - (-elapsed_ms / TAU_MS).exp());
            if (target - next).abs() < ARRIVED {
                target
            } else {
                next
            }
        };
        self.shown = Some(next);
        next.round() != before
    }

    /// A glide starts (a wheel notch): the next step measures time from `now_us`, not from the last frame
    /// of an earlier glide.
    pub fn start(&mut self, target_before: i32, now_us: u64) {
        if self.shown.is_none() {
            self.shown = Some(f64::from(target_before));
        }
        if !self.moving(target_before) {
            self.last_us = Some(now_us);
        }
    }

    /// The target moved by `by` for a reason other than the wheel (the page scrolled by itself): the
    /// picture follows at once.
    pub fn follow(&mut self, by: i32) {
        if let Some(shown) = self.shown.as_mut() {
            *shown += f64::from(by);
        }
    }

    /// Nothing is known about where the page is (a new size, a page's process gone).
    pub fn reset(&mut self) {
        self.shown = None;
        self.last_us = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(glide: &mut Glide, target: i32, from_us: u64, frames: u32) -> Vec<i32> {
        (1..=frames)
            .map(|n| {
                glide.step(target, from_us + u64::from(n) * 16_667);
                glide.top(target)
            })
            .collect()
    }

    #[test]
    fn a_notch_slides_over_a_few_frames_and_arrives() {
        let mut glide = Glide::default();
        glide.step(1000, 0);
        glide.start(1000, 0);
        let tops = run(&mut glide, 1040, 0, 12);
        assert!(tops[0] > 1000 && tops[0] < 1030, "{tops:?}");
        assert!(tops.windows(2).all(|w| w[1] >= w[0]), "{tops:?}");
        assert_eq!(*tops.last().unwrap(), 1040, "{tops:?}");
        assert!(!glide.moving(1040));
    }

    #[test]
    fn the_page_moving_by_itself_is_followed_at_once() {
        let mut glide = Glide::default();
        glide.step(500, 0);
        glide.follow(3000);
        assert_eq!(glide.top(3500), 3500);
        assert!(!glide.moving(3500));
    }

    #[test]
    fn a_stalled_window_does_not_leap() {
        let mut glide = Glide::default();
        glide.step(0, 0);
        glide.start(0, 0);
        glide.step(400, 2_000_000);
        // At most one capped step (50 ms of a 45 ms time constant) of the way.
        assert!(glide.top(400) < 300, "{}", glide.top(400));
    }

    #[test]
    fn without_a_glide_the_target_is_shown() {
        let glide = Glide::default();
        assert_eq!(glide.top(77), 77);
        assert!(!glide.moving(77));
    }
}
