//! A small deterministic random number generator.
//!
//! Every generator stage takes its own [`Rng`] derived from the course seed
//! and the stage's name ([`Rng::stage`]), so changing how one stage draws
//! numbers never reshuffles another.

/// SplitMix64: tiny, fast, and good enough for terrain and placement.
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng {
            state: seed ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    /// The sub-generator for one stage of the course `seed`.
    pub fn stage(seed: u64, stage: &str) -> Rng {
        Rng::new(seed ^ hash(stage.as_bytes()).rotate_left(17))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        mix(self.state)
    }

    /// A float in `0.0..1.0`.
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// A float in `lo..hi`.
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next_f32()
    }

    /// An integer in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (((self.next_u64() >> 32) * n as u64) >> 32) as usize
    }

    /// A standard normal sample (Box-Muller).
    pub fn gaussian(&mut self) -> f32 {
        let u = self.next_f32().max(1e-7);
        let v = self.next_f32();
        (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos()
    }

    pub fn chance(&mut self, p: f32) -> bool {
        self.next_f32() < p
    }
}

/// The SplitMix64 finaliser: a good 64-bit bit mixer.
pub fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// FNV-1a, for stage names.
pub fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xCBF2_9CE4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01B3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_are_independent_and_repeatable() {
        let a: Vec<u64> = (0..4).map(|_| Rng::stage(7, "trees").next_u64()).collect();
        assert!(a.windows(2).all(|w| w[0] == w[1]));
        assert_ne!(
            Rng::stage(7, "trees").next_u64(),
            Rng::stage(7, "routing").next_u64()
        );
        assert_ne!(
            Rng::stage(7, "trees").next_u64(),
            Rng::stage(8, "trees").next_u64()
        );
    }

    #[test]
    fn ranges_hold() {
        let mut rng = Rng::new(1);
        for _ in 0..10_000 {
            let f = rng.next_f32();
            assert!((0.0..1.0).contains(&f));
            assert!(rng.below(7) < 7);
        }
    }
}
