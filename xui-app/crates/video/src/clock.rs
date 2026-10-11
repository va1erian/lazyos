//! The media clock and the decision it drives: show a frame now, wait for
//! it, or drop it because the player fell behind. Time is passed in
//! (microseconds on any monotonic scale), so nothing here sleeps.
//!
//! When audio plays, its position replaces this clock as the master and the
//! same [`schedule`] decides what happens to each frame.

/// A frame later than this many frame durations is dropped, not shown.
pub const LATE_FRAMES: u64 = 2;

/// What to do with the next frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Schedule {
    /// Due (or slightly late): decode and show it.
    Show,
    /// Not due for this many microseconds.
    Wait(u64),
    /// Too late to be worth showing: skip to the frame the clock is at.
    Drop,
}

/// Decides `pts_us` against the clock at `position_us`, for frames
/// `frame_us` apart.
pub fn schedule(pts_us: u64, position_us: u64, frame_us: u64) -> Schedule {
    if pts_us > position_us {
        Schedule::Wait(pts_us - position_us)
    } else if position_us - pts_us > LATE_FRAMES * frame_us.max(1) {
        Schedule::Drop
    } else {
        Schedule::Show
    }
}

/// A pausable media clock anchored to the caller's time.
#[derive(Clone, Debug, Default)]
pub struct Clock {
    /// Media time at the anchor.
    media_us: u64,
    /// The caller's time at the anchor.
    anchor_us: u64,
    running: bool,
}

impl Clock {
    pub fn new() -> Clock {
        Clock::default()
    }

    pub fn running(&self) -> bool {
        self.running
    }

    /// Media time at `now_us`.
    pub fn position(&self, now_us: u64) -> u64 {
        if self.running {
            self.media_us + now_us.saturating_sub(self.anchor_us)
        } else {
            self.media_us
        }
    }

    pub fn play(&mut self, now_us: u64) {
        if !self.running {
            self.anchor_us = now_us;
            self.running = true;
        }
    }

    pub fn pause(&mut self, now_us: u64) {
        self.media_us = self.position(now_us);
        self.running = false;
    }

    /// Moves the clock to `media_us`, keeping it running or paused.
    pub fn seek(&mut self, media_us: u64, now_us: u64) {
        self.media_us = media_us;
        self.anchor_us = now_us;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: u64 = 33_333;

    #[test]
    fn a_frame_in_the_future_waits_exactly_that_long() {
        assert_eq!(schedule(100_000, 60_000, FRAME), Schedule::Wait(40_000));
    }

    #[test]
    fn a_slightly_late_frame_is_still_shown() {
        assert_eq!(schedule(0, FRAME, FRAME), Schedule::Show);
        assert_eq!(schedule(0, 2 * FRAME, FRAME), Schedule::Show);
        assert_eq!(schedule(10, 10, FRAME), Schedule::Show);
    }

    #[test]
    fn a_frame_more_than_two_durations_late_is_dropped() {
        assert_eq!(schedule(0, 2 * FRAME + 1, FRAME), Schedule::Drop);
    }

    #[test]
    fn a_zero_frame_duration_never_divides_or_underflows() {
        assert_eq!(schedule(5, 5, 0), Schedule::Show);
        assert_eq!(schedule(0, 10, 0), Schedule::Drop);
    }

    #[test]
    fn the_clock_runs_pauses_and_resumes_without_a_jump() {
        let mut c = Clock::new();
        assert_eq!(c.position(1_000), 0);
        c.play(1_000);
        assert_eq!(c.position(1_500), 500);
        c.pause(2_000);
        assert_eq!(c.position(9_000), 1_000);
        c.play(10_000);
        assert_eq!(c.position(10_250), 1_250);
    }

    #[test]
    fn a_seek_moves_a_running_and_a_paused_clock() {
        let mut c = Clock::new();
        c.play(0);
        c.seek(5_000, 100);
        assert_eq!(c.position(150), 5_050);
        c.pause(150);
        c.seek(2_000, 500);
        assert_eq!(c.position(900), 2_000);
    }

    #[test]
    fn time_going_backwards_does_not_underflow() {
        let mut c = Clock::new();
        c.play(1_000);
        assert_eq!(c.position(500), 0);
    }
}
