//! Seeded tests of the gamepad fuzz entry ([`super::run`]).

use super::*;
use fuzzkit::{for_seeds, Rng};

/// Op bytes of the script grammar that the generators below use.
const LAG: u8 = 11;
const DRAIN: u8 = 12;
const TABLE: u8 = 6;

/// Replay every checked-in seed (`fuzz/seeds/inputmap_gamepad`) and saved
/// crash (`fuzz/regressions/inputmap_gamepad`).
#[test]
fn checked_in_seeds_replay() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
    let mut seen = 0;
    for dir in ["seeds", "regressions"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir).join("inputmap_gamepad")) else {
            continue;
        };
        for entry in entries.flatten() {
            run(&std::fs::read(entry.path()).unwrap());
            seen += 1;
        }
    }
    if std::env::var_os("CI").is_some() {
        assert!(seen > 0, "no seeds found for inputmap_gamepad");
    }
}

/// Random scripts over eight device ids.
#[test]
fn random_scripts() {
    for_seeds(
        "inputmap::fuzz_gamepad::random_scripts",
        |_, rng: &mut Rng| {
            let len = rng.below(1500) as usize;
            let data: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
            run(&data);
        },
    );
}

/// Scripts biased to register pads first, so the engine sees traffic.
#[test]
fn pad_heavy_scripts() {
    for_seeds("inputmap::fuzz_gamepad::pad_heavy", |_, rng: &mut Rng| {
        let mut data = Vec::new();
        for device in 0..6u8 {
            data.extend([4, device, GAMEPAD_CLASS]);
        }
        let len = rng.below(1500) as usize;
        for _ in 0..len {
            // Mostly buttons, axes and flushes.
            let op = [0u8, 1, 2, 3, 7, 0, 2, 8][rng.below(8) as usize];
            data.push(op | (rng.byte() & 0x80));
            for _ in 0..5 {
                data.push(rng.byte());
            }
        }
        run(&data);
    });
}

/// The ring lags: pads come and go and the table is read ahead of the records
/// still queued, drained now and then.
#[test]
fn lagging_scripts() {
    for_seeds("inputmap::fuzz_gamepad::lagging", |_, rng: &mut Rng| {
        let mut data = vec![LAG, 0];
        for device in 0..5u8 {
            data.extend([4, device, GAMEPAD_CLASS]);
        }
        data.extend([DRAIN, 0]);
        let len = rng.below(1500) as usize;
        for _ in 0..len {
            // Buttons, axes, registrations and closes (some lost), table
            // reads ahead of the stream, the odd drain and loss marker.
            let op = [0u8, 1, 2, 4, 5, 5, 6, 6, 9, 12, 8, 0][rng.below(12) as usize];
            data.push(op | (rng.byte() & 0x80));
            for _ in 0..5 {
                data.push(rng.byte());
            }
        }
        data.extend([TABLE, 0, DRAIN, 0]);
        run(&data);
    });
}
