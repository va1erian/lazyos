//! Voices and the stereo mix: 16.16 fixed-point resampling, integer gain.

use crate::module::{Sample, CHANNELS};
use crate::tables::PAL_CLOCK;

/// One hardware-style channel: a sample cursor plus gain.
#[derive(Clone, Copy, Debug, Default)]
pub struct Voice {
    pub sample: Option<usize>,
    /// Position in sample frames, 16.16 fixed point.
    pos: u64,
    /// Frames advanced per output frame, 16.16.
    inc: u64,
    /// `0..=64`.
    pub volume: u8,
}

impl Voice {
    /// Start `sample` from its first frame.
    pub fn restart(&mut self, sample: Option<usize>) {
        self.sample = sample;
        self.pos = 0;
    }

    /// Start `offset` frames in (effect `9xx`); past the end it falls silent.
    pub fn seek(&mut self, offset: usize) {
        self.pos = (offset as u64) << 16;
    }

    /// Playback speed for a Paula `period` at `rate` Hz. Paula fetches one
    /// byte every `2 * period` clocks.
    pub fn set_period(&mut self, period: u16, rate: u32) {
        self.inc = if period == 0 {
            0
        } else {
            (PAL_CLOCK << 16) / (2 * u64::from(period) * u64::from(rate.max(1)))
        };
    }

    /// The current output (`-8192..=8128`: 8-bit sample times volume) and
    /// advance by one output frame.
    fn next(&mut self, samples: &[Sample], interpolate: bool) -> i32 {
        let Some(sample) = self.sample.and_then(|i| samples.get(i)) else {
            self.sample = None;
            return 0;
        };
        let index = (self.pos >> 16) as usize;
        let Some(&a) = sample.data.get(index) else {
            self.sample = None; // ran off the end of a one-shot sample
            return 0;
        };
        let a = i32::from(a);
        let value = if interpolate {
            let following = match sample.loop_range {
                Some((start, end)) if index + 1 >= end => sample.data.get(start),
                _ => sample.data.get(index + 1),
            };
            let b = following.map_or(a, |&b| i32::from(b));
            let frac = ((self.pos >> 8) & 0xFF) as i32;
            a + (((b - a) * frac) >> 8)
        } else {
            a
        };
        self.pos += self.inc;
        if let Some((start, end)) = sample.loop_range {
            let end_pos = (end as u64) << 16;
            if self.pos >= end_pos {
                let span = ((end - start) as u64) << 16; // start < end, so non-zero
                self.pos = ((start as u64) << 16) + (self.pos - end_pos) % span;
            }
        }
        value * i32::from(self.volume)
    }
}

/// Per-channel stereo weights out of 256, derived from the separation.
#[derive(Clone, Copy, Debug)]
pub struct Pan([(i32, i32); CHANNELS]);

impl Pan {
    /// Amiga layout (L R R L). `separation` is `0` (mono) to `100` (hard pan).
    pub fn new(separation: u8) -> Pan {
        let bleed = i32::from(100 - separation.min(100)) * 128 / 100;
        let left = (256 - bleed, bleed);
        let right = (bleed, 256 - bleed);
        Pan([left, right, right, left])
    }
}

fn narrow(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

/// Mix `out` (interleaved stereo) from the four voices.
///
/// Worst case two full-scale voices per side give `2 * 8192 * 256 >> 7`, which
/// fits `i16` exactly; the clamp covers the rounding corners.
pub fn mix(
    voices: &mut [Voice; CHANNELS],
    samples: &[Sample],
    pan: &Pan,
    interpolate: bool,
    out: &mut [i16],
) {
    for frame in out.chunks_exact_mut(2) {
        let (mut left, mut right) = (0i32, 0i32);
        for (voice, &(wl, wr)) in voices.iter_mut().zip(pan.0.iter()) {
            let value = voice.next(samples, interpolate);
            left += value * wl;
            right += value * wr;
        }
        frame[0] = narrow(left >> 7);
        frame[1] = narrow(right >> 7);
    }
}
