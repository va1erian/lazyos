//! Fuzz entry point, shared by the seeded tests below and the cargo-fuzz
//! target (`fuzz/fuzz_targets/usbaudio.rs`), so a crash found by one
//! replays under the other.
//!
//! [`run_desc`]: any bytes as a configuration chain, as a `GET_RANGE`
//! reply and as feedback reports. Nothing may panic, every accepted
//! alternate must be usable playback, every clock walk must end, and no
//! pace may exceed the payload limit.

use crate::choose::{choose, function_rates};
use crate::desc::parse_functions;
use crate::packet::Pacer;
use crate::request::parse_ranges;
use crate::STANDARD_RATES;

pub fn run_desc(data: &[u8]) {
    let ranges = parse_ranges(data).ok();
    let Ok(mut functions) = parse_functions(data) else {
        return;
    };
    for function in &mut functions {
        for alt in &function.alts {
            assert!(!alt.data.is_in(), "playback data endpoint is OUT");
            assert!(alt.data.max_packet != 0 && alt.data.max_packet <= 0x07FF);
            assert!((1..=3).contains(&alt.data.transactions));
            assert!((1..=8).contains(&alt.format.channels));
            assert!((2..=4).contains(&alt.format.subslot));
        }
        for clock in function.clocks() {
            if let Some(rates) = &ranges {
                function.set_clock_rates(clock, rates);
            }
        }
        let mask = function_rates(function);
        assert!(mask < 1 << STANDARD_RATES.len());
        for wanted in [8000, 44100, 48000, 192000, 1] {
            if let Some(choice) = choose(function, wanted, &[]) {
                let alt = &function.alts[choice.alt];
                assert!(alt.rates.contains(choice.rate));
                let mut pacer = Pacer::new(choice.rate, 8, 64, false);
                let _ = pacer.feedback(data.get(..4).unwrap_or(data));
                assert!((0..16).all(|_| pacer.next_frames() <= 64));
            }
        }
    }
}

#[cfg(test)]
mod seeded {
    use super::*;
    use fuzzkit::{for_seeds, Rng};

    fn replay(target: &str, run: fn(&[u8])) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join(target)) else {
                continue;
            };
            for entry in entries.flatten() {
                run(&std::fs::read(entry.path()).unwrap());
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for {target}");
        }
    }

    #[test]
    fn checked_in_seeds_replay() {
        replay("usbaudio", run_desc);
    }

    /// Mutations of the three golden chains: mostly valid structure with a
    /// few hostile bytes, which is where parsers break.
    #[test]
    fn mutated_chains() {
        let golden = [
            crate::tests::fixtures::qemu(),
            crate::tests::fixtures::headset(),
            crate::tests::fixtures::uac2_dac(),
        ];
        for_seeds("usbaudio::fuzz::mutated_chains", |_, rng: &mut Rng| {
            let mut data = golden[rng.below(3) as usize].clone();
            for _ in 0..1 + rng.below(4) {
                let at = rng.below(data.len() as u64) as usize;
                data[at] = rng.byte();
            }
            if rng.one_in(4) {
                let cut = rng.below(data.len() as u64) as usize;
                data.truncate(cut);
            }
            run_desc(&data);
        });
    }

    #[test]
    fn random_bytes() {
        for_seeds("usbaudio::fuzz::random_bytes", |_, rng: &mut Rng| {
            let len = rng.below(300) as usize;
            run_desc(&rng.bytes(len));
        });
    }
}
