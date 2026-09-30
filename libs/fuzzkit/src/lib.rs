//! Deterministic, seeded fuzz-style testing that runs inside plain
//! `cargo test` on every platform.
//!
//! A libFuzzer target and an in-tree test share one entry point: a function
//! that takes `&[u8]`, interprets it as a script of operations against a
//! reference model, and panics on any violated invariant. This crate supplies
//! the second half: a small PRNG to make those byte strings, and a driver that
//! runs many seeds and, when one fails, prints the seed and how to replay it.
//!
//! * `FUZZ_SEED=<n>` (decimal or `0x` hex) runs exactly that seed, the replay
//!   knob a failure message points at;
//! * `FUZZ_CASES=<n>` sets the number of seeds per test (default
//!   [`DEFAULT_CASES`]); CI can raise it for a soak.
//!
//! This is a host-only crate; the fuzzed libraries stay `no_std`.

use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};

/// Seeds run per test when `FUZZ_CASES` is not set.
pub const DEFAULT_CASES: u64 = 256;

/// xoshiro256** seeded through splitmix64. Fast, well distributed, and stable
/// across platforms, which is all a reproducible fuzz seed needs.
#[derive(Clone, Debug)]
pub struct Rng {
    s: [u64; 4],
}

impl Rng {
    pub fn new(seed: u64) -> Rng {
        let mut x = seed;
        let mut next = || {
            x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        Rng {
            s: [next(), next(), next(), next()],
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    pub fn byte(&mut self) -> u8 {
        (self.next_u64() >> 56) as u8
    }

    /// A value in `0..bound` (`bound == 0` gives 0). The modulo bias is
    /// irrelevant for test input.
    pub fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            0
        } else {
            self.next_u64() % bound
        }
    }

    /// A value in `lo..=hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }

    /// `true` one time in `n`.
    pub fn one_in(&mut self, n: u64) -> bool {
        self.below(n) == 0
    }

    pub fn fill(&mut self, out: &mut [u8]) {
        for byte in out {
            *byte = self.byte();
        }
    }

    /// `len` random bytes.
    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = vec![0; len];
        self.fill(&mut out);
        out
    }

    /// A random slice of `items`.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }

    /// Flip `flips` random bits of `data` in place (no-op on empty input).
    pub fn flip_bits(&mut self, data: &mut [u8], flips: usize) {
        if data.is_empty() {
            return;
        }
        for _ in 0..flips {
            let bit = self.below(data.len() as u64 * 8) as usize;
            data[bit / 8] ^= 1 << (bit % 8);
        }
    }
}

fn parse_u64(text: &str) -> Option<u64> {
    let text = text.trim();
    match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

/// The base seed for a named test: `FUZZ_SEED` when set (replay), otherwise a
/// stable hash of `name`, so an unset environment always runs the same seeds
/// and a red CI run is a red local run.
fn base_seed(name: &str) -> (u64, bool) {
    if let Some(seed) = std::env::var("FUZZ_SEED")
        .ok()
        .as_deref()
        .and_then(parse_u64)
    {
        return (seed, true);
    }
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    for byte in name.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3);
    }
    (hash, false)
}

/// Run `body` once per seed. On a panic, print the failing seed (and the
/// environment variable that replays it) before re-raising, so the failure can
/// be reproduced exactly.
pub fn for_seeds(name: &str, mut body: impl FnMut(u64, &mut Rng)) {
    let (base, replay) = base_seed(name);
    let cases = if replay {
        1
    } else {
        std::env::var("FUZZ_CASES")
            .ok()
            .as_deref()
            .and_then(parse_u64)
            .unwrap_or(DEFAULT_CASES)
    };
    for case in 0..cases {
        let seed = if replay {
            base
        } else {
            Rng::new(base.wrapping_add(case)).next_u64()
        };
        let mut rng = Rng::new(seed);
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| body(seed, &mut rng))) {
            eprintln!(
                "\nFUZZ FAILURE in `{name}`: seed = {seed:#018x}\n  replay: FUZZ_SEED={seed:#x} cargo test {name}\n"
            );
            resume_unwind(panic);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic_and_spread() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        let mut c = Rng::new(8);
        let xs: Vec<u64> = (0..64).map(|_| a.next_u64()).collect();
        assert_eq!(xs, (0..64).map(|_| b.next_u64()).collect::<Vec<_>>());
        assert_ne!(xs, (0..64).map(|_| c.next_u64()).collect::<Vec<_>>());
        // Every byte value shows up in a modest sample.
        let mut seen = [false; 256];
        for _ in 0..20_000 {
            seen[a.byte() as usize] = true;
        }
        assert!(seen.iter().all(|s| *s));
    }

    #[test]
    fn bounded_helpers_stay_in_range() {
        let mut rng = Rng::new(1);
        for _ in 0..10_000 {
            assert!(rng.below(10) < 10);
            let v = rng.range(5, 9);
            assert!((5..=9).contains(&v));
        }
        assert_eq!(rng.below(0), 0);
    }

    #[test]
    fn a_failing_seed_is_reported_and_propagates() {
        let result = catch_unwind(|| {
            for_seeds("fuzzkit::selftest", |_, rng| {
                rng.next_u64();
            });
            for_seeds("fuzzkit::selftest_fail", |_, _| panic!("boom"));
        });
        assert!(result.is_err());
    }

    #[test]
    fn seeds_parse_in_decimal_and_hex() {
        assert_eq!(parse_u64("42"), Some(42));
        assert_eq!(parse_u64("0x2a"), Some(42));
        assert_eq!(parse_u64("nope"), None);
    }
}
