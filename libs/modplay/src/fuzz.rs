//! Byte-level fuzzing of the parser and player.
//!
//! [`run`] is the shared entry point for the cargo-fuzz target
//! (`fuzz/fuzz_targets/modplay.rs`) and the seeded tests below. It must never
//! panic or allocate without bound, whatever the bytes.

use std::vec;

use crate::{Module, Options, Player};

/// Frames rendered per input: enough to cross rows and effects, bounded so
/// one case stays fast.
const FRAMES: usize = 48_000 * 4;

/// Parse `input` and, if it is a module, render a few seconds of it.
pub fn run(input: &[u8]) {
    let Ok(module) = Module::parse(input) else {
        return;
    };
    let mut player = Player::new(
        &module,
        48_000,
        Options {
            loops: None,
            ..Options::default()
        },
    );
    let mut buf = vec![0i16; 2 * 1024];
    let mut done = 0;
    while done < FRAMES {
        let n = player.render(&mut buf);
        if n == 0 {
            break;
        }
        done += n;
    }
}

#[cfg(test)]
mod seeded {
    use super::*;
    use crate::synth::{effect, note, square, ModBuilder};
    use fuzzkit::for_seeds;

    fn valid(rng: &mut fuzzkit::Rng) -> std::vec::Vec<u8> {
        let mut builder = ModBuilder::new()
            .sample(1, square(32), 64, true)
            .sample(2, square(64), 40, false)
            .orders(&[0, 1, 0]);
        for _ in 0..rng.range(4, 40) {
            let (pattern, row, channel) = (
                rng.below(2) as usize,
                rng.below(64) as usize,
                rng.below(4) as usize,
            );
            let cell = crate::Note {
                period: rng.range(100, 900) as u16,
                sample: rng.below(4) as u8,
                effect: rng.below(16) as u8,
                param: rng.byte(),
            };
            builder = builder.note(pattern, row, channel, cell);
        }
        builder
            .note(0, 0, 0, note(428, 1))
            .note(0, 1, 0, effect(0xC, 30))
            .build()
    }

    #[test]
    fn mutated_valid_modules_are_safe() {
        for_seeds("modplay::mutated_valid_modules_are_safe", |_, rng| {
            let mut data = valid(rng);
            let flips = rng.range(1, 16) as usize;
            rng.flip_bits(&mut data, flips);
            run(&data);
        });
    }

    #[test]
    fn random_effects_in_valid_modules_are_safe() {
        for_seeds(
            "modplay::random_effects_in_valid_modules_are_safe",
            |_, rng| {
                run(&valid(rng));
            },
        );
    }

    #[test]
    fn arbitrary_bytes_with_a_signature_are_safe() {
        for_seeds(
            "modplay::arbitrary_bytes_with_a_signature_are_safe",
            |_, rng| {
                let len = rng.range(0, 5000) as usize;
                let mut data = rng.bytes(len);
                if data.len() > 1084 {
                    data[1080..1084].copy_from_slice(b"M.K.");
                    data[950] = rng.range(1, 129) as u8;
                }
                run(&data);
            },
        );
    }
}
