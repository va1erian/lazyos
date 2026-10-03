//! Where a deck's frames go: the system mixer (`super::mixer`), or a clock
//! when there is none.
//!
//! A [`Sink`] never blocks: the deck asks how much room there is
//! ([`Sink::free`]), renders that much and hands it over. Frame counts are
//! totals over the deck's life, across pauses, so the deck can map "frames
//! played" back to the song position it rendered there.

use std::time::Instant;

/// Mixing rate asked of the mixer. The mixer caps a stream's ring at 64 KiB,
/// so at 22.05 kHz stereo the ring holds about 0.74 s: the UI thread may stall
/// that long (a slow repaint) before the music skips.
pub const DEFAULT_RATE: u32 = 22_050;

/// How much a clock sink buffers, in milliseconds.
const CLOCK_BUFFER_MS: u64 = 500;

/// A destination for interleaved stereo `i16` frames.
pub trait Sink {
    /// Frames per second it plays.
    fn rate(&self) -> u32;
    /// Whether this is a real audio stream (`false`: a silent clock).
    fn is_audio(&self) -> bool;
    /// Frames it can take now without blocking.
    fn free(&mut self) -> Result<usize, String>;
    /// Queue whole frames; returns how many it took.
    fn write(&mut self, samples: &[i16]) -> Result<usize, String>;
    /// Total frames played so far (as of the last [`Sink::free`]).
    fn played(&self) -> u64;
    /// Total frames handed over so far.
    fn written(&self) -> u64;
    /// Start playing what is queued even though the buffer is not full (the
    /// end of a song shorter than the buffer).
    fn flush(&mut self) -> Result<(), String>;
    /// Stop playing, keeping what was queued but not played for
    /// [`Sink::resume`].
    fn pause(&mut self) -> Result<(), String>;
    fn resume(&mut self) -> Result<(), String>;
    /// Output volume, `0..=100`.
    fn set_volume(&mut self, percent: u8) -> Result<(), String>;
}

/// Milliseconds since some fixed point; injectable for tests.
pub type Clock = Box<dyn Fn() -> u64>;

/// The real clock.
pub fn wall_clock() -> Clock {
    let origin = Instant::now();
    Box::new(move || origin.elapsed().as_millis() as u64)
}

/// A silent sink that plays at wall-clock speed: the deck behaves the same
/// (positions advance, the song ends) on an image without a sound card.
pub struct ClockSink {
    rate: u32,
    clock: Clock,
    capacity: u64,
    written: u64,
    played: u64,
    /// The clock reading `played` was last advanced to.
    at: u64,
    paused: bool,
    started: bool,
}

impl ClockSink {
    pub fn new(rate: u32, clock: Clock) -> ClockSink {
        let at = clock();
        ClockSink {
            rate,
            clock,
            capacity: u64::from(rate) * CLOCK_BUFFER_MS / 1000,
            written: 0,
            played: 0,
            at,
            paused: false,
            started: false,
        }
    }

    /// Advance `played` by the time elapsed, never past what was written.
    fn advance(&mut self) {
        let now = (self.clock)();
        if !self.paused && self.started {
            let frames = now.saturating_sub(self.at) * u64::from(self.rate) / 1000;
            if frames > 0 {
                self.played = (self.played + frames).min(self.written);
                self.at += frames * 1000 / u64::from(self.rate);
            }
            if self.played == self.written {
                self.at = now; // drained: time spent empty is not owed later
            }
        } else {
            self.at = now;
        }
    }
}

impl Sink for ClockSink {
    fn rate(&self) -> u32 {
        self.rate
    }

    fn is_audio(&self) -> bool {
        false
    }

    fn free(&mut self) -> Result<usize, String> {
        self.advance();
        Ok((self.capacity - (self.written - self.played)) as usize)
    }

    fn write(&mut self, samples: &[i16]) -> Result<usize, String> {
        let room = (self.capacity - (self.written - self.played)) as usize;
        let frames = (samples.len() / 2).min(room);
        if !self.started {
            self.at = (self.clock)();
            self.started = true;
        }
        self.written += frames as u64;
        Ok(frames)
    }

    fn played(&self) -> u64 {
        self.played
    }

    fn written(&self) -> u64 {
        self.written
    }

    fn flush(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn pause(&mut self) -> Result<(), String> {
        self.advance();
        self.paused = true;
        Ok(())
    }

    fn resume(&mut self) -> Result<(), String> {
        self.paused = false;
        self.at = (self.clock)();
        Ok(())
    }

    fn set_volume(&mut self, _percent: u8) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    fn manual() -> (Rc<Cell<u64>>, Clock) {
        let now = Rc::new(Cell::new(1_000));
        let reader = Rc::clone(&now);
        (now, Box::new(move || reader.get()))
    }

    #[test]
    fn a_clock_sink_plays_at_its_rate_and_never_past_what_was_written() {
        let (now, clock) = manual();
        let mut sink = ClockSink::new(1000, clock);
        assert_eq!(sink.free().unwrap(), 500);
        assert_eq!(
            sink.write(&[0; 2 * 600]).unwrap(),
            500,
            "capped at its buffer"
        );
        now.set(now.get() + 100);
        assert_eq!(sink.free().unwrap(), 100);
        assert_eq!(sink.played(), 100);
        now.set(now.get() + 10_000);
        sink.free().unwrap();
        assert_eq!(sink.played(), 500, "drained, not past the end");
        assert_eq!(sink.written(), 500);
    }

    #[test]
    fn a_paused_clock_sink_does_not_advance() {
        let (now, clock) = manual();
        let mut sink = ClockSink::new(1000, clock);
        sink.write(&[0; 2 * 400]).unwrap();
        now.set(now.get() + 50);
        sink.pause().unwrap();
        now.set(now.get() + 1_000);
        sink.free().unwrap();
        assert_eq!(sink.played(), 50);
        sink.resume().unwrap();
        now.set(now.get() + 50);
        sink.free().unwrap();
        assert_eq!(sink.played(), 100);
    }

    #[test]
    fn a_clock_sink_does_not_count_time_before_the_first_write() {
        let (now, clock) = manual();
        let mut sink = ClockSink::new(1000, clock);
        now.set(now.get() + 5_000);
        sink.free().unwrap();
        sink.write(&[0; 2 * 300]).unwrap();
        now.set(now.get() + 100);
        sink.free().unwrap();
        assert_eq!(sink.played(), 100);
    }
}
