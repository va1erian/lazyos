//! Streaming sample-rate conversion by linear interpolation, in 32.32 fixed
//! point.
//!
//! The converter keeps two input frames, `cur` and `next`, and a fractional
//! position between them. Each output frame is the interpolation at that
//! position; then the position advances by `in_rate / out_rate` and every
//! whole step *pops* one input frame (`cur = next`, `next = input`). How many
//! frames a run of outputs pops is therefore known in advance
//! ([`Resampler::input_needed`]), so the mixer checks a stream has enough
//! committed before touching it, and a stream converted in many small pieces
//! produces exactly the samples one large piece would.
//!
//! Linear interpolation is the cheapest converter that keeps pitch exact; it
//! lets some aliasing through, which is acceptable for system sounds and
//! games. Equal rates bypass it entirely, bit for bit.

/// 1.0 in 32.32 fixed point.
const ONE: u64 = 1 << 32;

/// Input frames per output frame never exceeds this (192 kHz into 8 kHz is
/// 24), so a run of outputs cannot overflow the 64-bit position.
const MAX_STEP: u64 = 64 * ONE;

#[derive(Clone, Debug)]
pub struct Resampler {
    /// Input frames per output frame, 32.32.
    step: u64,
    /// Position between `cur` and `next`, in `0..ONE`.
    frac: u64,
    cur: [i32; 2],
    next: [i32; 2],
}

impl Resampler {
    /// A converter from `in_rate` to `out_rate` (both in Hz; zero is treated
    /// as 1 Hz, which no grant produces).
    pub fn new(in_rate: u32, out_rate: u32) -> Resampler {
        let step = (u64::from(in_rate.max(1)) << 32) / u64::from(out_rate.max(1));
        Resampler {
            step: step.clamp(1, MAX_STEP),
            frac: 0,
            cur: [0; 2],
            next: [0; 2],
        }
    }

    /// Whether input and output rates are equal (frames are copied as is).
    pub fn is_passthrough(&self) -> bool {
        self.step == ONE
    }

    /// Forget the history, as after a `Stop`: the next output starts from
    /// silence.
    pub fn reset(&mut self) {
        self.frac = 0;
        self.cur = [0; 2];
        self.next = [0; 2];
    }

    /// Input frames [`Resampler::process`] pops to produce `outputs` frames.
    pub fn input_needed(&self, outputs: usize) -> u64 {
        if self.is_passthrough() {
            return outputs as u64;
        }
        let total = u128::from(self.frac) + u128::from(self.step) * outputs as u128;
        (total >> 32) as u64
    }

    /// Convert interleaved `input` of `channels` (1 or 2; mono is copied to
    /// both sides) into stereo frames in `out`, stopping when `out` is full or
    /// the next output would pop more input than is left. Returns `(input
    /// frames consumed, output frames produced)`.
    pub fn process(
        &mut self,
        input: &[i16],
        channels: usize,
        out: &mut [[i16; 2]],
    ) -> (usize, usize) {
        let channels = channels.clamp(1, 2);
        let frames = input.len() / channels;
        let frame = |index: usize| -> [i32; 2] {
            let base = index * channels;
            let left = i32::from(input[base]);
            let right = i32::from(input[base + channels - 1]);
            [left, right]
        };
        if self.is_passthrough() {
            let count = frames.min(out.len());
            for (index, slot) in out[..count].iter_mut().enumerate() {
                let [left, right] = frame(index);
                *slot = [left as i16, right as i16];
            }
            return (count, count);
        }
        let (mut used, mut produced) = (0, 0);
        while produced < out.len() {
            let advanced = self.frac + self.step;
            let pops = (advanced >> 32) as usize;
            if used + pops > frames {
                break;
            }
            out[produced] = self.interpolate();
            produced += 1;
            for _ in 0..pops {
                self.cur = self.next;
                self.next = frame(used);
                used += 1;
            }
            self.frac = advanced & (ONE - 1);
        }
        (used, produced)
    }

    /// The frame at the current position between `cur` and `next`.
    fn interpolate(&self) -> [i16; 2] {
        let lerp = |a: i32, b: i32| {
            let delta = i64::from(b - a) * self.frac as i64;
            // Between two i16 values, so it fits.
            (i64::from(a) + (delta >> 32)) as i16
        };
        [
            lerp(self.cur[0], self.next[0]),
            lerp(self.cur[1], self.next[1]),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn sine(rate: u32, freq: u32, frames: usize) -> Vec<i16> {
        // A test-only float sine (host tests may use std).
        (0..frames)
            .map(|n| {
                let t = n as f64 / f64::from(rate);
                (12000.0 * (2.0 * core::f64::consts::PI * f64::from(freq) * t).sin()) as i16
            })
            .collect()
    }

    /// Frequency estimate from rising zero crossings of the left channel.
    fn pitch(frames: &[[i16; 2]], rate: u32) -> f64 {
        let crossings: Vec<usize> = frames
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[0][0] < 0 && w[1][0] >= 0)
            .map(|(i, _)| i)
            .collect();
        let span = (crossings[crossings.len() - 1] - crossings[0]) as f64;
        (crossings.len() - 1) as f64 * f64::from(rate) / span
    }

    fn convert_all(resampler: &mut Resampler, input: &[i16], channels: usize) -> Vec<[i16; 2]> {
        let mut out = vec![[0i16; 2]; input.len() * 30 + 16];
        let (used, produced) = resampler.process(input, channels, &mut out);
        assert!(used <= input.len() / channels);
        out.truncate(produced);
        out
    }

    #[test]
    fn equal_rates_copy_bit_for_bit() {
        let mut resampler = Resampler::new(48000, 48000);
        assert!(resampler.is_passthrough());
        let input = [1i16, -2, 3, -4, i16::MAX, i16::MIN];
        let out = convert_all(&mut resampler, &input, 2);
        assert_eq!(out, [[1, -2], [3, -4], [i16::MAX, i16::MIN]]);
        let mono = convert_all(&mut resampler, &[7, -7], 1);
        assert_eq!(mono, [[7, 7], [-7, -7]]);
        assert_eq!(resampler.input_needed(1024), 1024);
    }

    #[test]
    fn doubling_the_rate_interpolates_midpoints() {
        let mut resampler = Resampler::new(24000, 48000);
        let out = convert_all(&mut resampler, &[0, 100, 200, 300], 1);
        // Two input frames of priming silence (`cur` and `next` start at
        // zero), then the ramp at half steps; the last output waits for a
        // fifth input frame.
        let left: Vec<i16> = out.iter().map(|f| f[0]).collect();
        assert_eq!(left, [0, 0, 0, 0, 0, 50, 100, 150, 200]);
    }

    #[test]
    fn pitch_is_preserved_across_common_conversions() {
        for (from, to) in [
            (44100, 48000),
            (22050, 48000),
            (96000, 48000),
            (8000, 48000),
        ] {
            let input = sine(from, 440, from as usize);
            let mut resampler = Resampler::new(from, to);
            let mut out = vec![[0i16; 2]; to as usize * 2];
            let (_, produced) = resampler.process(&input, 1, &mut out);
            let measured = pitch(&out[..produced], to);
            assert!(
                (measured - 440.0).abs() < 2.0,
                "{from}->{to}: {measured} Hz"
            );
        }
    }

    #[test]
    fn input_needed_predicts_exactly_what_process_pops() {
        for (from, to) in [
            (44100, 48000),
            (8000, 48000),
            (192000, 8000),
            (11025, 44100),
        ] {
            let mut resampler = Resampler::new(from, to);
            let input = vec![5i16; 30_000 * 2];
            for outputs in [1usize, 7, 64, 1024, 333] {
                let needed = resampler.input_needed(outputs) as usize;
                let mut out = vec![[0i16; 2]; outputs];
                let (used, produced) = resampler.process(&input[..needed * 2], 2, &mut out);
                assert_eq!(
                    (used, produced),
                    (needed, outputs),
                    "{from}->{to} x{outputs}"
                );
            }
        }
    }

    #[test]
    fn chunked_conversion_equals_one_shot() {
        let input = sine(44100, 1000, 9000);
        let mut whole = Resampler::new(44100, 48000);
        let expected = convert_all(&mut whole, &input, 1);

        let mut pieces = Resampler::new(44100, 48000);
        let mut got = Vec::new();
        let mut at = 0;
        let mut chunk = 1;
        while at < input.len() {
            let end = (at + chunk).min(input.len());
            let mut out = vec![[0i16; 2]; 4096];
            let (used, produced) = pieces.process(&input[at..end], 1, &mut out);
            got.extend_from_slice(&out[..produced]);
            at += used;
            if used == 0 {
                chunk += 1;
            }
            chunk = chunk % 97 + 1;
        }
        assert_eq!(got.len(), expected.len());
        assert_eq!(got, expected);
    }

    #[test]
    fn short_input_stops_cleanly() {
        let mut resampler = Resampler::new(8000, 48000);
        let mut out = vec![[0i16; 2]; 100];
        let (used, produced) = resampler.process(&[], 2, &mut out);
        assert_eq!(used, 0);
        // Upsampling emits a few outputs before the first pop is due.
        assert!(produced <= 6);
        let mut downsample = Resampler::new(192000, 8000);
        let (used, produced) = downsample.process(&[1, 2, 3, 4], 2, &mut out);
        assert_eq!((used, produced), (0, 0));
    }

    #[test]
    fn extreme_values_do_not_overflow() {
        let mut resampler = Resampler::new(11025, 48000);
        let input: Vec<i16> = (0..4000)
            .map(|n| if n % 2 == 0 { i16::MAX } else { i16::MIN })
            .collect();
        let out = convert_all(&mut resampler, &input, 1);
        assert!(out.len() > 4000);
    }
}
