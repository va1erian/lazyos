//! How smooth scrolling looks from the window: `INFO:SMOOTH:` once a second while the wheel turns.
//!
//! The page's answer (`INFO:PAINT`'s `w2d`) and the window's own wheel-to-draw time (`INFO:SPEC`) say how
//! late a notch is seen; this says what a person watching sees: how many frames the page view drew in a
//! second of scrolling, the longest gap between two of them, how many gaps were longer than a 30 fps
//! frame, and how much of what was drawn was checkerboard (rows the page had not painted yet).
//!
//!   `INFO:SMOOTH:wheel=<events>:frames=<draws>:gapmax=<us>:over33=<gaps>:shown=<rows drawn>:checker=<rows>`
//!
//! A gap counts only while scrolling is under way (a wheel event that moved the picture in the last
//! [`ACTIVE_MS`]), so the pause after a burst is not jank, nor is the wheel turning against the end of
//! the page, where there is nothing to draw. `wheel` counts every event. Numbers only, so it is tested
//! on the host.

/// Scrolling is under way this long after a wheel event, or while the picture moves.
pub const ACTIVE_MS: u64 = 250;
/// A gap longer than this is a frame a 30 fps display would have missed.
pub const JANK_US: u64 = 33_333;

#[derive(Debug, Default)]
pub struct Smooth {
    since_ms: u64,
    last_wheel_ms: Option<u64>,
    /// When the scrolling under way began: a gap is never counted from before it.
    burst_us: u64,
    last_draw_us: Option<u64>,
    wheel: u32,
    frames: u32,
    gap_max_us: u64,
    over: u32,
    shown: u64,
    checker: u64,
}

impl Smooth {
    /// A wheel event at `now_ms`; `moved` when it moved what is shown (not at the end of the page).
    pub fn wheel(&mut self, now_ms: u64, moved: bool) {
        self.wheel += 1;
        if moved {
            if !self.active(now_ms, false) {
                self.burst_us = now_ms.saturating_mul(1000);
            }
            self.last_wheel_ms = Some(now_ms);
        }
    }

    fn active(&self, now_ms: u64, moving: bool) -> bool {
        moving
            || self
                .last_wheel_ms
                .is_some_and(|at| now_ms.saturating_sub(at) <= ACTIVE_MS)
    }

    /// The page view was drawn at `now_us` (`now_ms` the same instant), `rows` high of which `checker`
    /// rows were checkerboard; `moving` while the picture is shown away from where the page is.
    pub fn drawn(&mut self, now_us: u64, rows: u32, checker: u32, moving: bool) {
        let now_ms = now_us / 1000;
        let active = self.active(now_ms, moving);
        if let (true, Some(last)) = (active, self.last_draw_us) {
            let gap = now_us.saturating_sub(last.max(self.burst_us));
            self.gap_max_us = self.gap_max_us.max(gap);
            self.over += u32::from(gap > JANK_US);
        }
        self.last_draw_us = Some(now_us);
        if active {
            self.frames += 1;
            self.shown += u64::from(rows);
            self.checker += u64::from(checker.min(rows));
        }
    }

    /// The line for the last second at `now_ms`, if a second has passed and the wheel turned in it.
    pub fn line(&mut self, now_ms: u64) -> Option<String> {
        if now_ms.saturating_sub(self.since_ms) < 1000 {
            return None;
        }
        let line = (self.wheel > 0 || self.frames > 0).then(|| {
            format!(
                "INFO:SMOOTH:wheel={}:frames={}:gapmax={}:over33={}:shown={}:checker={}",
                self.wheel, self.frames, self.gap_max_us, self.over, self.shown, self.checker
            )
        });
        *self = Smooth {
            since_ms: now_ms,
            last_wheel_ms: self.last_wheel_ms,
            burst_us: self.burst_us,
            last_draw_us: self.last_draw_us,
            ..Smooth::default()
        };
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaps_count_only_while_scrolling() {
        let mut s = Smooth::default();
        s.line(0);
        s.wheel(1000, true);
        s.drawn(1_005_000, 900, 0, false);
        s.drawn(1_021_000, 900, 100, false);
        s.drawn(1_071_000, 900, 0, false); // a 50 ms gap: jank
                                           // Long after the wheel: neither counted nor a gap.
        s.drawn(1_900_000, 900, 0, false);
        let line = s.line(2000).unwrap();
        assert_eq!(
            line,
            "INFO:SMOOTH:wheel=1:frames=3:gapmax=50000:over33=1:shown=2700:checker=100"
        );
        assert_eq!(s.line(3000), None);
    }

    #[test]
    fn the_first_gap_of_a_burst_counts_from_the_first_notch() {
        let mut s = Smooth::default();
        s.line(0);
        s.drawn(100_000, 900, 0, false); // idle, long before the wheel
        s.wheel(1000, true);
        s.drawn(1_020_000, 900, 0, false);
        let line = s.line(2000).unwrap();
        assert_eq!(
            line,
            "INFO:SMOOTH:wheel=1:frames=1:gapmax=20000:over33=0:shown=900:checker=0"
        );
    }

    #[test]
    fn the_wheel_against_the_end_of_the_page_is_not_scrolling() {
        let mut s = Smooth::default();
        s.line(0);
        s.drawn(900_000, 900, 0, false);
        s.wheel(1000, false);
        s.wheel(1100, false);
        s.drawn(1_200_000, 900, 0, false);
        let line = s.line(2000).unwrap();
        assert_eq!(
            line,
            "INFO:SMOOTH:wheel=2:frames=0:gapmax=0:over33=0:shown=0:checker=0"
        );
    }

    #[test]
    fn a_moving_picture_counts_without_the_wheel() {
        let mut s = Smooth::default();
        s.line(0);
        s.drawn(5_000_000, 10, 20, true);
        s.drawn(5_016_000, 10, 0, true);
        let line = s.line(6000).unwrap();
        assert_eq!(
            line,
            "INFO:SMOOTH:wheel=0:frames=2:gapmax=16000:over33=0:shown=20:checker=10"
        );
    }
}
