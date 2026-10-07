//! Byte-level fuzzing of `elevd`'s operation table.
//!
//! [`run`] is the shared entry point for the cargo-fuzz target
//! (`fuzz/fuzz_targets/elevpolicy.rs`) and the seeded tests below. The bytes
//! are a request as `elevd` receives it after the wire decoding (which
//! `libs/accountwire` fuzzes): an operation name and its arguments, separated
//! by NUL bytes. Whatever they are, [`Operation::parse`] must not panic; what
//! it accepts must name a table row, re-parse from its own arguments to the
//! same operation, and have a prompt summary that never carries a password.

use alloc::string::String;
use alloc::vec::Vec;

use crate::{Operation, NAMES};

/// Split `input` into an operation name and its arguments, then check.
pub fn run(input: &[u8]) {
    let text = String::from_utf8_lossy(input);
    let mut parts = text.split('\0');
    let name = parts.next().unwrap_or("");
    let args: Vec<String> = parts.map(String::from).collect();
    let Ok(op) = Operation::parse(name, &args) else {
        return;
    };
    assert!(NAMES.contains(&op.name()));
    assert_eq!(op.name(), name);
    assert_eq!(Operation::parse(op.name(), &op.args()), Ok(op.clone()));
    let summary = op.summary();
    assert!(!summary.is_empty());
    // The password never shapes the prompt text: the same operation with
    // another password reads the same.
    let mut other = op.clone();
    if let Operation::AccountCreate { secret, .. } | Operation::AccountPassword { secret, .. } =
        &mut other
    {
        *secret = String::from("another password");
        assert_eq!(
            other.summary(),
            summary,
            "a password reached the prompt text"
        );
    }
}

#[cfg(test)]
mod seeded {
    use super::*;
    use fuzzkit::for_seeds;

    /// Replay every checked-in seed (`fuzz/seeds/elevpolicy`) and saved crash
    /// (`fuzz/regressions/elevpolicy`).
    #[test]
    fn corpus_and_regressions_replay() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join("elevpolicy")) else {
                continue;
            };
            for entry in entries.flatten() {
                run(&std::fs::read(entry.path()).unwrap());
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for elevpolicy");
        }
    }

    #[test]
    fn generated_requests_are_safe() {
        let args: [&[u8]; 14] = [
            b"admin",
            b"bob",
            b"_x",
            b"user",
            b"1",
            b"0",
            b"keep",
            b"remove",
            b"str",
            b"bytes",
            b"sys/ui/demo",
            b"/transient/a.lzp",
            b"4102444800",
            b"",
        ];
        for_seeds("elevpolicy::generated_requests_are_safe", |_, rng| {
            let mut request = rng.pick(NAMES).as_bytes().to_vec();
            for _ in 0..rng.range(0, 5) {
                request.push(0);
                request.extend(*rng.pick(&args));
            }
            run(&request);
            rng.flip_bits(&mut request, 2);
            run(&request);
        });
    }

    #[test]
    fn raw_noise_is_safe() {
        for_seeds("elevpolicy::raw_noise_is_safe", |_, rng| {
            let len = rng.range(0, 96) as usize;
            run(&rng.bytes(len));
        });
    }
}
