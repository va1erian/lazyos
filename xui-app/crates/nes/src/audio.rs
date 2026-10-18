//! The console's samples as the mixer takes them.

/// The gain applied to the console's mix.
///
/// `tetanes_core`'s APU reproduces the NES's non-linear mixer (the NESdev
/// tables) and scales the result so that even everything at once stays inside
/// ±0.2: a single full-volume pulse peaks near 0.06, which is faithful and far
/// quieter than the rest of the system (`beep` plays at 0.49 of full scale).
/// Five times the mix puts one pulse near a third of full scale and a loud
/// frame just under it, where the mixer's own volume control can take it.
pub const OUTPUT_GAIN: f32 = 5.0;

/// `samples` as `S16Le`, appended to `out`.
pub fn to_i16(samples: &[f32], gain: f32, out: &mut Vec<i16>) {
    out.extend(samples.iter().map(|sample| {
        let value = f64::from(*sample) * f64::from(gain) * 32_767.0;
        value.clamp(-32_768.0, 32_767.0) as i16
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_silence() {
        let mut out = Vec::new();
        to_i16(&[0.0, 0.0], OUTPUT_GAIN, &mut out);
        assert_eq!(out, vec![0, 0]);
    }

    #[test]
    fn the_gain_scales_and_clamps() {
        let mut out = Vec::new();
        to_i16(&[0.1, -0.1, 1.0, -1.0], 5.0, &mut out);
        assert_eq!(out[0], (0.5 * 32_767.0) as i16);
        assert_eq!(out[1], (-0.5 * 32_767.0) as i16);
        assert_eq!(out[2], 32_767);
        assert_eq!(out[3], -32_768);
    }

    #[test]
    fn samples_append() {
        let mut out = vec![1, 2];
        to_i16(&[0.0], OUTPUT_GAIN, &mut out);
        assert_eq!(out, vec![1, 2, 0]);
    }
}
