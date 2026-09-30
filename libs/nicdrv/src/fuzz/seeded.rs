//! The seeded tests: the same entry point as the libFuzzer target, driven by a
//! PRNG over many seeds inside plain `cargo test`.

use std::vec::Vec;

use super::*;
use fuzzkit::{for_seeds, Rng};

/// A script biased toward traffic, with hostile ops only when asked.
fn script(rng: &mut Rng, len: usize, hostile: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(len * 4);
    out.push(rng.byte());
    // Attach first, so traffic has somewhere to go.
    out.extend_from_slice(&[190, 0, 0]);
    for _ in 0..len {
        let mut op = rng.byte();
        if !hostile && ((60..=69).contains(&op) || (220..=239).contains(&op)) {
            op = rng.below(60) as u8;
        }
        out.push(op);
        let extra = match op {
            0..=59 => 4,
            60..=69 => 40,
            100..=139 => 3,
            190..=229 => 8,
            230..=239 => 10,
            _ => 2,
        };
        for _ in 0..extra {
            out.push(rng.byte());
        }
    }
    out
}

#[test]
fn clean_scripts_match_the_model() {
    for_seeds("nicdrv::clean_scripts_match_the_model", |_, rng| {
        let len = rng.range(50, 1500) as usize;
        run(&script(rng, len, false));
    });
}

#[test]
fn hostile_scripts_are_safe() {
    for_seeds("nicdrv::hostile_scripts_are_safe", |_, rng| {
        let len = rng.range(50, 1500) as usize;
        run(&script(rng, len, true));
    });
}

#[test]
fn arbitrary_bytes_are_safe() {
    for_seeds("nicdrv::arbitrary_bytes_are_safe", |_, rng| {
        let len = rng.range(0, 4000) as usize;
        run(&rng.bytes(len));
    });
}

#[test]
fn checked_in_seeds_replay() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
    let mut seen = 0;
    for dir in ["seeds", "regressions"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir).join("nicdrv")) else {
            continue;
        };
        for entry in entries.flatten() {
            run(&std::fs::read(entry.path()).unwrap());
            seen += 1;
        }
    }
    if std::env::var_os("CI").is_some() {
        assert!(seen > 0, "no seeds found for nicdrv");
    }
}
