//! Byte-level fuzzing of the account file parser.
//!
//! [`run`] is the shared entry point for the cargo-fuzz target
//! (`fuzz/fuzz_targets/passwd.rs`) and the seeded tests below. Whatever the
//! bytes, [`parse`](crate::parse) must not panic, must fail closed (an error,
//! never a partial list), and what it accepts must satisfy every rule the
//! rest of the system relies on and survive a serialise/parse round trip.

use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};

use crate::{parse, valid_name, Entry, LoadError, IN_SHADOW, NAME_MAX, PASSWD_MAX};

/// The rows as a file `parse` accepts.
fn serialise(entries: &[Entry]) -> String {
    let mut out = String::new();
    for e in entries {
        out.push_str(&alloc::format!(
            "{}:{}:{}:{}:{}:{}\n",
            e.name,
            e.uid,
            e.gid,
            IN_SHADOW,
            e.home,
            e.shell
        ));
    }
    out
}

/// Whether `home` is an absolute path with no empty, `.` or `..` component.
fn normalised(home: &str) -> bool {
    home.starts_with('/')
        && (home.len() == 1 || home[1..].split('/').all(|p| !matches!(p, "" | "." | "..")))
}

fn check_accepted(entries: &[Entry]) {
    assert!(!entries.is_empty(), "an empty account list is an error");
    let mut names = BTreeSet::new();
    let mut uids = BTreeSet::new();
    for e in entries {
        assert!(valid_name(&e.name) && e.name.len() <= NAME_MAX);
        assert!(names.insert(e.name.clone()), "duplicate name accepted");
        assert!(uids.insert(e.uid), "duplicate uid accepted");
        assert!(normalised(&e.home) && !e.home.chars().any(char::is_control));
        assert!(!e.shell.is_empty());
        assert!(!e.shell.chars().any(|c| c.is_whitespace() || c.is_control()));
    }
    // What it accepted, written back out, parses to the same accounts (when
    // it still fits the size cap: the input may have been padded).
    let text = serialise(entries);
    if text.len() <= PASSWD_MAX {
        assert_eq!(parse(text.as_bytes()).as_deref(), Ok(entries));
    }
}

/// Parse `input` and check the invariants. Never panics.
pub fn run(input: &[u8]) {
    let result = parse(input);
    if input.len() > PASSWD_MAX {
        assert_eq!(result, Err(LoadError::Oversize(input.len())));
        return;
    }
    if core::str::from_utf8(input).is_err() {
        assert_eq!(result, Err(LoadError::NotText));
        return;
    }
    match result {
        Ok(entries) => check_accepted(&entries),
        // The reason text is what `ACCOUNTS:LOAD:FAIL` prints.
        Err(error) => assert!(!error.to_string().is_empty()),
    }
}

#[cfg(test)]
mod seeded {
    use super::*;
    use fuzzkit::{for_seeds, Rng};
    use std::vec::Vec;

    /// Replay every checked-in seed (`fuzz/seeds/passwd`) and saved crash
    /// (`fuzz/regressions/passwd`).
    #[test]
    fn corpus_and_regressions_replay() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join("passwd")) else {
                continue;
            };
            for entry in entries.flatten() {
                run(&std::fs::read(entry.path()).unwrap());
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for passwd");
        }
    }

    fn field(rng: &mut Rng, alphabet: &[u8], max: u64) -> Vec<u8> {
        (0..rng.below(max)).map(|_| *rng.pick(alphabet)).collect()
    }

    fn id(rng: &mut Rng) -> Vec<u8> {
        let pool: [&[u8]; 6] = [b"0", b"1000", b"4294967295", b"4294967296", b"-1", b"1 "];
        rng.pick(&pool).to_vec()
    }

    /// A plausible row, each field good or hostile.
    fn row(rng: &mut Rng) -> Vec<u8> {
        let homes: [&[u8]; 6] = [
            b"/",
            b"/home/a",
            b"/home/../x",
            b"home/a",
            b"/a//b",
            b"/./a",
        ];
        let mut out = field(rng, b"abcdefgxyz_-019ABC", 40);
        let parts = [
            id(rng),
            id(rng),
            // Mostly the only accepted value, `x`; anything else must fail.
            if rng.one_in(3) {
                field(rng, b"pw:\r #x", 6)
            } else {
                b"x".to_vec()
            },
            rng.pick(&homes).to_vec(),
            field(rng, b"/bin/sh \t\x01", 8),
        ];
        for part in parts {
            out.push(b':');
            out.extend(part);
        }
        out
    }

    #[test]
    fn generated_files_are_safe() {
        for_seeds("passwd::generated_files_are_safe", |_, rng| {
            let mut file = Vec::new();
            for _ in 0..rng.range(0, 8) {
                if rng.one_in(6) {
                    file.extend(b"# comment");
                } else {
                    file.extend(row(rng));
                }
                file.extend(*rng.pick(&[&b"\n"[..], b"\r\n", b"\n\n"]));
            }
            run(&file);
            rng.flip_bits(&mut file, 2);
            run(&file);
        });
    }

    #[test]
    fn raw_noise_is_safe() {
        for_seeds("passwd::raw_noise_is_safe", |_, rng| {
            let len = rng.range(0, PASSWD_MAX as u64 + 64) as usize;
            run(&rng.bytes(len));
        });
    }

    #[test]
    fn the_shipped_file_survives_mutation() {
        let shipped = include_bytes!("../../../build_support/passwd");
        for_seeds("passwd::the_shipped_file_survives_mutation", |_, rng| {
            let mut data = shipped.to_vec();
            let flips = rng.range(0, 6) as usize;
            rng.flip_bits(&mut data, flips);
            run(&data);
        });
    }
}
