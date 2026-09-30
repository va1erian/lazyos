//! Byte-level fuzzing of the package reader.
//!
//! [`run`] is the shared entry point for the cargo-fuzz target
//! (`fuzz/fuzz_targets/lazypkg.rs`) and the seeded tests below: it opens
//! `input` as a package and, if that succeeds, reads every entry and calls
//! `digest` and `install_dir`. It must never panic on any input, so every
//! operation returns a `Result` that is deliberately ignored.

/// Open `input` and exercise the whole validated surface. Never panics.
pub fn run(input: &[u8]) {
    let Ok(package) = crate::Package::open(input) else {
        return;
    };
    let _ = package.digest();
    let _ = package.install_dir();
    for entry in package.entries() {
        let _ = package.read(entry.name);
    }
}

#[cfg(test)]
mod seeded {
    use super::*;
    use crate::testzip;
    use fuzzkit::for_seeds;

    /// Replay every checked-in seed (`fuzz/seeds/lazypkg`) and saved crash
    /// (`fuzz/regressions/lazypkg`) so the corpus stays valid and a fixed bug
    /// stays fixed under plain `cargo test`.
    fn replay(target: &str) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join(target)) else {
                continue;
            };
            for entry in entries.flatten() {
                let data = std::fs::read(entry.path()).unwrap();
                run(&data);
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for {target}");
        }
    }

    #[test]
    fn mutated_valid_archives_are_safe() {
        for_seeds("lazypkg::mutated_valid_archives_are_safe", |_, rng| {
            let mut data = testzip::valid(rng.one_in(2));
            let flips = rng.range(1, 24) as usize;
            rng.flip_bits(&mut data, flips);
            run(&data);
        });
    }

    #[test]
    fn arbitrary_bytes_are_safe() {
        for_seeds("lazypkg::arbitrary_bytes_are_safe", |_, rng| {
            let len = rng.range(0, 4096) as usize;
            run(&rng.bytes(len));
        });
    }

    #[test]
    fn truncations_are_safe() {
        for deflate in [false, true] {
            let data = testzip::valid(deflate);
            for cut in 0..=data.len() {
                run(&data[..cut]);
            }
        }
    }

    #[test]
    fn checked_in_corpus_replays() {
        replay("lazypkg");
    }

    /// The checked-in valid seeds must actually open, so a regression in the
    /// Python seed writer (`fuzz/gen_corpus.py`) fails `cargo test` here.
    #[test]
    fn checked_in_valid_seeds_open() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/lazypkg");
        for name in ["valid_stored", "valid_deflated"] {
            let Ok(data) = std::fs::read(root.join(name)) else {
                continue;
            };
            if let Err(error) = crate::Package::open(&data) {
                panic!("{name}: {error}");
            }
        }
    }
}
