//! A fixed-point sine tone generator: no floating point (user programs are
//! built soft-float) and no libm, just a phase accumulator and a quarter-wave
//! polynomial.

/// Quarter of a full turn in phase units (the phase is a 32-bit turn).
const QUARTER: u32 = 1 << 30;

// sin(pi/2 * x) ~= C1*x + C3*x^3 + C5*x^5 on [0, 1], coefficients in Q30
// (a fit constrained to be exactly 1 at x = 1 with zero slope there, so the
// quadrants join smoothly; worst-case error about 2e-4 of full scale).
const C1: i64 = 1_685_428_436; //  1.56967755
const C3: i64 = -686_502_311; //  -0.63935510
const C5: i64 = 74_815_700; //     0.06967755

/// `sin` of a phase in `0..=QUARTER` (a quarter turn), as Q30 in `0..=2^30`.
fn quarter_sin(phase: u32) -> i64 {
    let x = i64::from(phase);
    let x2 = (x * x) >> 30;
    let x3 = (x2 * x) >> 30;
    let x5 = (x3 * x2) >> 30;
    let y = ((C1 * x) >> 30) + ((C3 * x3) >> 30) + ((C5 * x5) >> 30);
    y.clamp(0, 1 << 30)
}

/// Sine of a full-turn phase (`u32::MAX + 1` is 2 pi) as Q30, in `-2^30..=2^30`.
pub fn sin_q30(phase: u32) -> i32 {
    let quadrant = phase >> 30;
    let within = phase & (QUARTER - 1);
    let value = match quadrant {
        0 => quarter_sin(within),
        1 => quarter_sin(QUARTER - within),
        2 => -quarter_sin(within),
        _ => -quarter_sin(QUARTER - within),
    };
    value as i32
}

/// A tone at a fixed frequency and amplitude.
#[derive(Clone, Copy, Debug)]
pub struct Tone {
    phase: u32,
    step: u32,
    /// Peak amplitude as a fraction of full scale, in Q15 (`32767` is 1.0).
    amplitude: i32,
}

impl Tone {
    /// `freq_hz` at `rate_hz`; `amplitude_q15` is clamped to `0..=32767`.
    /// A frequency at or above Nyquist is clamped just below it.
    pub fn new(freq_hz: u32, rate_hz: u32, amplitude_q15: i32) -> Tone {
        let rate = u64::from(rate_hz.max(1));
        let freq = u64::from(freq_hz).min(rate / 2 - rate / 2 / 64);
        Tone {
            phase: 0,
            step: (freq << 32).div_ceil(rate).min(u64::from(u32::MAX)) as u32,
            amplitude: amplitude_q15.clamp(0, 32767),
        }
    }

    /// The next sample as a signed 16-bit value.
    pub fn next_sample(&mut self) -> i16 {
        let value = (i64::from(sin_q30(self.phase)) * i64::from(self.amplitude)) >> 30;
        self.phase = self.phase.wrapping_add(self.step);
        value.clamp(-32768, 32767) as i16
    }

    /// Fill `out` with interleaved 16-bit little-endian frames, the same sample
    /// on every channel. Returns the number of whole frames written; a trailing
    /// partial frame is left untouched.
    pub fn fill_s16le(&mut self, out: &mut [u8], channels: usize) -> usize {
        if channels == 0 {
            return 0;
        }
        let frame_bytes = 2 * channels;
        let frames = out.len() / frame_bytes;
        for frame in out.chunks_exact_mut(frame_bytes).take(frames) {
            let sample = self.next_sample().to_le_bytes();
            for channel in frame.as_chunks_mut::<2>().0 {
                *channel = sample;
            }
        }
        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    /// Reference sine of a full-turn phase, in Q30, from libm.
    fn reference(phase: u32) -> f64 {
        (f64::from(phase) / 4_294_967_296.0 * core::f64::consts::TAU).sin()
    }

    #[test]
    fn sine_tracks_libm_across_the_whole_turn() {
        let mut worst = 0.0f64;
        for step in 0..=4096u32 {
            let phase = step.wrapping_mul(1 << 20); // 4096 samples per turn
            let got = f64::from(sin_q30(phase)) / f64::from(1u32 << 30);
            worst = worst.max((got - reference(phase)).abs());
        }
        assert!(worst < 3e-4, "worst error {worst}");
    }

    #[test]
    fn sine_hits_the_exact_landmarks() {
        assert_eq!(sin_q30(0), 0);
        assert!((sin_q30(QUARTER) - (1 << 30)).abs() < 1 << 18);
        assert!((sin_q30(3 * QUARTER) + (1 << 30)).abs() < 1 << 18);
        assert!(sin_q30(2 * QUARTER).abs() < 1 << 18);
    }

    fn render(freq: u32, rate: u32, seconds_x10: u32, channels: usize) -> Vec<i16> {
        let mut tone = Tone::new(freq, rate, 16000);
        let frames = (rate * seconds_x10 / 10) as usize;
        let mut bytes = std::vec![0u8; frames * 2 * channels];
        assert_eq!(tone.fill_s16le(&mut bytes, channels), frames);
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .step_by(channels)
            .map(|pair| i16::from_le_bytes(*pair))
            .collect()
    }

    fn rising_zero_crossings(samples: &[i16]) -> usize {
        samples.windows(2).filter(|w| w[0] < 0 && w[1] >= 0).count()
    }

    #[test]
    fn frequency_matches_by_zero_crossings() {
        for (freq, rate) in [(440, 48000), (1000, 44100), (220, 48000), (3000, 96000)] {
            let samples = render(freq, rate, 10, 2);
            let crossings = rising_zero_crossings(&samples) as i64;
            assert!(
                (crossings - i64::from(freq)).abs() <= 1,
                "{freq} Hz at {rate}: {crossings}"
            );
        }
    }

    #[test]
    fn amplitude_is_the_requested_peak_and_channels_match() {
        let samples = render(440, 48000, 2, 2);
        let peak = samples.iter().map(|s| i32::from(*s).abs()).max().unwrap();
        assert!((15900..=16100).contains(&peak), "peak {peak}");
        let mut tone = Tone::new(440, 48000, 16000);
        let mut bytes = [0u8; 64];
        tone.fill_s16le(&mut bytes, 2);
        for frame in bytes.as_chunks::<4>().0 {
            assert_eq!(frame[..2], frame[2..]);
        }
    }

    #[test]
    fn output_is_not_silent_and_starts_at_zero() {
        let samples = render(440, 48000, 1, 1);
        assert_eq!(samples[0], 0);
        assert!(samples.iter().any(|s| s.abs() > 10000));
    }

    #[test]
    fn partial_frames_and_degenerate_arguments_are_safe() {
        let mut tone = Tone::new(440, 48000, 16000);
        let mut bytes = [0xAAu8; 7];
        assert_eq!(tone.fill_s16le(&mut bytes, 2), 1); // one 4-byte frame
        assert_eq!(&bytes[4..], &[0xAA; 3]); // the tail is left alone
        assert_eq!(tone.fill_s16le(&mut bytes, 0), 0);
        assert_eq!(Tone::new(440, 48000, -5).next_sample(), 0);
        // Nyquist and beyond, zero rate: clamped, no panic, no overflow.
        for (freq, rate) in [(24000, 48000), (u32::MAX, 48000), (440, 0), (0, 48000)] {
            let mut tone = Tone::new(freq, rate, i32::MAX);
            for _ in 0..1000 {
                tone.next_sample();
            }
        }
    }
}
