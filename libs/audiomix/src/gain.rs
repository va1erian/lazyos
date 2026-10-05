//! 16.16 fixed-point gain and saturation.
//!
//! A gain of [`UNITY`] leaves a sample unchanged; 0 silences it; the most a
//! client may ask for is [`MAX_Q16`] (four times unity, +12 dB), enough to lift
//! a quiet source without letting a typo ask for something absurd. Scaling
//! rounds toward negative infinity and never overflows: products are formed in
//! 64 bits and only the final mix saturates to 16 bits.

/// Unity gain in 16.16 fixed point.
pub const UNITY: u32 = 1 << 16;

/// The largest gain a client may set: four times unity.
pub const MAX_Q16: u32 = 4 * UNITY;

/// A validated gain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gain(u32);

impl Gain {
    pub const UNITY: Gain = Gain(UNITY);
    pub const SILENT: Gain = Gain(0);

    /// `q16` as a gain, or `None` above [`MAX_Q16`].
    pub fn new(q16: u32) -> Option<Gain> {
        (q16 <= MAX_Q16).then_some(Gain(q16))
    }

    /// A percentage of unity (100 is unity, at most 400).
    pub fn from_percent(percent: u32) -> Option<Gain> {
        Gain::new(percent.checked_mul(UNITY)? / 100)
    }

    /// The gain as a percentage of unity, rounded to the nearest.
    pub fn percent(self) -> u32 {
        (self.0 * 100 + UNITY / 2) / UNITY
    }

    pub fn q16(self) -> u32 {
        self.0
    }

    pub fn is_unity(self) -> bool {
        self.0 == UNITY
    }

    pub fn is_silent(self) -> bool {
        self.0 == 0
    }

    /// Scale one sample (any width up to 32 bits) without overflow.
    pub fn scale(self, sample: i32) -> i64 {
        (i64::from(sample) * i64::from(self.0)) >> 16
    }

    /// Scale an accumulated mix (wider than any sample) without overflow:
    /// the mix of every stream at maximum gain stays far below 2^47.
    pub fn scale_wide(self, value: i64) -> i64 {
        (value * i64::from(self.0)) >> 16
    }
}

/// Clamp a mixed value into the 16-bit range instead of wrapping.
pub fn saturate(value: i64) -> i16 {
    value.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
}

/// Scale a buffer of interleaved `S16Le` samples in place; a trailing odd
/// byte is left alone. Unity is a no-op, so the common case costs nothing.
pub fn scale_s16le(bytes: &mut [u8], gain: Gain) {
    if gain.is_unity() {
        return;
    }
    for pair in bytes.as_chunks_mut::<2>().0 {
        let sample = i16::from_le_bytes(*pair);
        *pair = saturate(gain.scale(i32::from(sample))).to_le_bytes();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unity_and_silence() {
        for sample in [i32::from(i16::MIN), -1, 0, 1, 12345, i32::from(i16::MAX)] {
            assert_eq!(Gain::UNITY.scale(sample), i64::from(sample));
            assert_eq!(Gain::SILENT.scale(sample), 0);
        }
    }

    #[test]
    fn half_gain_halves_and_rounds_down() {
        let half = Gain::new(UNITY / 2).unwrap();
        assert_eq!(half.scale(1000), 500);
        assert_eq!(half.scale(-1000), -500);
        assert_eq!(half.scale(1), 0);
        assert_eq!(half.scale(-1), -1);
    }

    #[test]
    fn max_gain_does_not_overflow_and_saturates_only_at_the_end() {
        let max = Gain::new(MAX_Q16).unwrap();
        assert_eq!(max.scale(i32::from(i16::MAX)), 4 * i64::from(i16::MAX));
        assert_eq!(max.scale(i32::MIN), 4 * i64::from(i32::MIN));
        assert_eq!(saturate(max.scale(i32::from(i16::MAX))), i16::MAX);
        assert_eq!(saturate(max.scale(i32::from(i16::MIN))), i16::MIN);
        // Every stream at maximum gain, then master at maximum gain.
        let worst = 64 * max.scale(i32::from(i16::MIN));
        assert_eq!(saturate(max.scale_wide(worst)), i16::MIN);
    }

    #[test]
    fn half_gain_is_minus_six_db() {
        // A full-scale-ish square of +-20000: the peak at half gain is -6.02 dB.
        let before = 20000f64;
        let after = Gain::new(UNITY / 2).unwrap().scale(20000) as f64;
        let db = 20.0 * (after / before).log10();
        assert!((db + 6.02).abs() < 0.01, "{db} dB");
    }

    #[test]
    fn out_of_range_gains_are_refused() {
        assert!(Gain::new(MAX_Q16 + 1).is_none());
        assert!(Gain::new(u32::MAX).is_none());
        assert!(Gain::from_percent(401).is_none());
        assert!(Gain::from_percent(u32::MAX).is_none());
    }

    #[test]
    fn percent_round_trips() {
        for percent in [0, 1, 25, 50, 99, 100, 150, 400] {
            assert_eq!(Gain::from_percent(percent).unwrap().percent(), percent);
        }
        assert_eq!(Gain::from_percent(50).unwrap().q16(), UNITY / 2);
    }

    #[test]
    fn s16le_buffers_scale_in_place() {
        let mut bytes = [0u8; 8];
        for (pair, sample) in
            bytes
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip([1000i16, -1000, i16::MAX, i16::MIN])
        {
            pair.copy_from_slice(&sample.to_le_bytes());
        }
        let mut doubled = bytes;
        scale_s16le(&mut doubled, Gain::new(2 * UNITY).unwrap());
        let read = |b: &[u8], i: usize| i16::from_le_bytes([b[2 * i], b[2 * i + 1]]);
        assert_eq!(
            [0, 1, 2, 3].map(|i| read(&doubled, i)),
            [2000, -2000, i16::MAX, i16::MIN]
        );
        let mut same = bytes;
        scale_s16le(&mut same, Gain::UNITY);
        assert_eq!(same, bytes);
        let mut odd = [0x10u8, 0x00, 0x7f];
        scale_s16le(&mut odd, Gain::SILENT);
        assert_eq!(odd, [0, 0, 0x7f]);
    }
}
