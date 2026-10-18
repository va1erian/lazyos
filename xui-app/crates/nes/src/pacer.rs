//! How many frames the emulator owes: the console runs at 60.0988 Hz, the
//! mixer drains at its own rate, and the window's timer fires when it likes.
//!
//! The pacer is pure arithmetic over the time the caller reports, so the
//! rules are host-tested: never run more than [`MAX_CATCH_UP`] frames in one
//! call (a stall must not become a burst that stalls again), never run while
//! the audio ring has no room for a frame (that is what keeps the picture and
//! the sound in step), and never drop the leftover fraction of a frame.

use std::time::Duration;

/// The NTSC frame period: 60.0988 frames a second.
pub const FRAME_NANOS: u64 = 16_639_267;
/// Most frames one call runs, so a stalled window catches up smoothly.
pub const MAX_CATCH_UP: usize = 3;

/// The debt of frames the emulator owes the clock.
#[derive(Debug, Default)]
pub struct Pacer {
    /// Time not yet spent on a frame, less than one frame's worth after a
    /// call that ran something.
    behind: Duration,
}

impl Pacer {
    pub fn new() -> Pacer {
        Pacer::default()
    }

    /// Forgets any debt (a ROM was loaded, or the window was away).
    pub fn reset(&mut self) {
        self.behind = Duration::ZERO;
    }

    /// How many frames to clock now: `elapsed` since the last call, `speed`
    /// the emulation speed, and `free` the audio ring's room in frames when
    /// there is a stream (`None` when there is none, or it is muted).
    /// `frame_samples` is how many samples one display frame produces.
    pub fn due(
        &mut self,
        elapsed: Duration,
        speed: f64,
        free: Option<usize>,
        frame_samples: usize,
    ) -> usize {
        let speed = if speed.is_finite() && speed > 0.0 {
            speed
        } else {
            1.0
        };
        let period = Duration::from_nanos((FRAME_NANOS as f64 / speed) as u64);
        let budget = self.behind + elapsed;
        let frames = (budget.as_nanos() / period.as_nanos().max(1)) as usize;
        let mut due = frames.min(MAX_CATCH_UP);
        if let Some(free) = free {
            // The ring's room bounds the burst too: writing more frames than
            // it can take would drop their samples.
            due = due.min(free / frame_samples.max(1));
        }
        self.behind = if due == 0 {
            // Keep at most one frame of debt while waiting for time or room:
            // after a long stall the console resumes smoothly rather than
            // racing to catch up.
            budget.min(period)
        } else if frames > MAX_CATCH_UP {
            Duration::ZERO
        } else {
            budget - period.mul_f64(due as f64)
        };
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn a_frame_is_due_after_a_frame_period() {
        let mut pacer = Pacer::new();
        assert_eq!(pacer.due(ms(16), 1.0, None, 0), 0);
        assert_eq!(pacer.due(ms(1), 1.0, None, 0), 1);
        // The fraction of a frame is kept: two more halves make one.
        let mut pacer = Pacer::new();
        assert_eq!(pacer.due(ms(8), 1.0, None, 0), 0);
        assert_eq!(pacer.due(ms(8), 1.0, None, 0), 0);
        assert_eq!(pacer.due(ms(1), 1.0, None, 0), 1);
    }

    #[test]
    fn a_stall_is_capped_and_not_raced() {
        let mut pacer = Pacer::new();
        assert_eq!(pacer.due(ms(1000), 1.0, None, 0), MAX_CATCH_UP);
        // The debt is gone (the cap dropped it), so the next frame waits.
        assert_eq!(pacer.due(ms(1), 1.0, None, 0), 0);
        assert_eq!(pacer.due(ms(16), 1.0, None, 0), 1);
    }

    #[test]
    fn a_full_audio_ring_pauses_the_console() {
        let mut pacer = Pacer::new();
        // 800 samples a frame: room for one is not room for a frame.
        assert_eq!(pacer.due(ms(100), 1.0, Some(799), 800), 0);
        assert_eq!(pacer.due(ms(1), 1.0, Some(800), 800), 1);
        // A stalled ring does not lose the debt.
        let mut pacer = Pacer::new();
        assert_eq!(pacer.due(ms(50), 1.0, Some(0), 800), 0);
        assert_eq!(pacer.due(ms(1), 1.0, Some(800), 800), 1);
    }

    #[test]
    fn fast_forward_runs_twice_as_many_frames() {
        let mut pacer = Pacer::new();
        // 16.6 ms of wall clock is one frame at 1x and two at 2x.
        assert_eq!(pacer.due(ms(17), 2.0, None, 0), 2);
    }
}
