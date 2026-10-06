//! Byte-level fuzzing of the package permission pipeline: manifest text ->
//! validation -> [`rules::compile`] -> [`explain::permissions`] (issue #626).
//!
//! `pkgd` is a root service and a manifest is attacker-controlled, so what it
//! accepts decides which kernel policy rules an app gets and what the consent
//! screen tells the user. [`run`] is the shared entry point for the cargo-fuzz
//! target (`fuzz/fuzz_targets/pkgstore_rules.rs`) and the seeded tests below.
//! Whatever the bytes, nothing may panic, and an accepted manifest must satisfy:
//!
//! * the rule list is at most [`rules::MAX_RULES`] long, holds no duplicate,
//!   allows only, and is the same on every call; a refusal names a count
//!   beyond the limit;
//! * no `files` entry has a `..` segment or a `**` and every one is a `read:`
//!   or `write:` rule; no topic has a `**` or a dot-only segment (`.`, `..`);
//! * every permission is listed once, in order, with a known risk word and a
//!   non-empty sentence.
//!
//! Input that is not a manifest is used a second way: its lines become
//! permission entries of a valid manifest, so the fuzzer reaches the policy
//! code without first having to write TOML.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::explain::{self, HIGH, LOW, MEDIUM};
use crate::rules::{self, CompileError, MAX_RULES};

/// The fixed part of the manifest the line-based path completes.
const HEADER: &str = "[app]\nname = \"Demo\"\nsystem_name = \"org.lazy.demo\"\nauthor = \"A\"\n\
                      version = \"1.0.0\"\n[entry]\nbinary = \"bin/app.elf\"\n";

/// Parse and check `text`; whether it was an accepted manifest.
fn check_manifest(text: &str) -> bool {
    let Ok(manifest) = lazypkg::parse_manifest(text) else {
        return false;
    };
    let wanted = &manifest.permissions;

    match rules::compile(&manifest) {
        Ok(list) => {
            assert!(list.len() <= MAX_RULES, "{} rules", list.len());
            assert!(list.iter().all(|rule| rule.allow));
            for (index, rule) in list.iter().enumerate() {
                assert!(!list[..index].contains(rule), "duplicate rule");
            }
            assert_eq!(rules::compile(&manifest), Ok(list.clone()), "unstable");
            let installed = rules::installed(&manifest);
            assert!(installed.is_ok_and(|rules| rules.len() <= MAX_RULES));
        }
        Err(CompileError::TooManyRules { rules }) => assert!(rules > MAX_RULES),
    }

    for entry in &wanted.files {
        let path = entry
            .strip_prefix("read:")
            .or_else(|| entry.strip_prefix("write:"));
        let path = path.expect("an accepted files entry is read: or write:");
        assert!(!path.contains("**"), "{entry}");
        assert!(path.split('/').all(|segment| segment != ".."), "{entry}");
    }
    for entry in &wanted.topics {
        assert!(!entry.contains("**"), "{entry}");
        let pattern = entry.split_once(':').map_or(entry.as_str(), |(_, rest)| rest);
        assert!(
            pattern.split('/').all(|segment| !segment.bytes().all(|b| b == b'.')),
            "{entry}"
        );
    }

    let shown = explain::permissions(&manifest);
    let count = wanted.interfaces.len()
        + wanted.topics.len()
        + wanted.files.len()
        + wanted.network.len()
        + usize::from(wanted.develop);
    assert_eq!(
        shown.len(),
        count,
        "a permission is missing from the consent list"
    );
    for permission in &shown {
        assert!([LOW, MEDIUM, HIGH].contains(&permission.risk.as_str()));
        assert!(!permission.explanation.is_empty() && !permission.kind.is_empty());
    }
    true
}

/// Quote `line` as a TOML basic string, or `None` when it would need escapes
/// (such a line could only make the manifest fail to parse).
fn quoted(line: &str) -> Option<String> {
    if line
        .chars()
        .any(|c| c == '"' || c == '\\' || c.is_control())
    {
        return None;
    }
    Some(format!("\"{line}\""))
}

/// The lines of `text` as the permission lists of a valid manifest, sorted
/// into `topics`, `files`, `network` and `interfaces` by their prefix.
fn manifest_from_lines(text: &str) -> String {
    let (mut interfaces, mut topics, mut files, mut network) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for line in text.lines().take(400) {
        let Some(item) = quoted(line) else {
            continue;
        };
        if line.starts_with("publish:") || line.starts_with("subscribe:") {
            topics.push(item);
        } else if line.starts_with("read:") || line.starts_with("write:") {
            files.push(item);
        } else if line.starts_with("net") || line == "outbound" {
            network.push(item);
        } else {
            interfaces.push(item);
        }
    }
    format!(
        "{HEADER}[permissions]\ninterfaces = [{}]\ntopics = [{}]\nfiles = [{}]\nnetwork = [{}]\n\
         develop = {}\n",
        interfaces.join(","),
        topics.join(","),
        files.join(","),
        network.join(","),
        text.len() & 1 == 0
    )
}

/// Run the pipeline on `input`. Never panics on a correct implementation.
pub fn run(input: &[u8]) {
    let Ok(text) = core::str::from_utf8(input) else {
        return;
    };
    if check_manifest(text) {
        return;
    }
    // The explanation table sees raw, unvalidated entries too (an installed
    // manifest is re-read at boot): it must answer for any text.
    for line in text.lines().take(400) {
        for explained in [
            explain::interface(line),
            explain::topic(line),
            explain::file(line, "org.lazy.demo"),
            explain::network(line),
        ] {
            assert!([LOW, MEDIUM, HIGH].contains(&explained.risk));
            assert!(!explained.text.is_empty());
        }
        let _ = rules::service_names(line);
    }
    check_manifest(&manifest_from_lines(text));
}

#[cfg(test)]
mod seeded {
    use super::*;
    use fuzzkit::{for_seeds, Rng};

    /// Replay every checked-in seed (`fuzz/seeds/pkgstore_rules`) and saved
    /// crash (`fuzz/regressions/pkgstore_rules`).
    #[test]
    fn corpus_and_regressions_replay() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join("pkgstore_rules")) else {
                continue;
            };
            for entry in entries.flatten() {
                run(&std::fs::read(entry.path()).unwrap());
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for pkgstore_rules");
        }
    }

    const INTERFACES: &[&str] = &[
        "os.lazy.confd.v1",
        "os.lazy.clipboard.v1",
        "os.lazy.net.socket.v1",
        "os.lazy.accounts.v1",
        "os.lazy.nonexistent.v1",
        "os.lazy.init.v1",
    ];
    const PATTERNS: &[&str] = &[
        "publish:app/org.lazy.demo/x",
        "publish:sys/#",
        "subscribe:+/+/#",
        "subscribe:a/**",
        "publish:a/../b",
        "publish:#/x",
        "publish:",
        "read:$HOME/*",
        "write:$HOME/docs/**",
        "write:/system/bin/*",
        "read:/a/../b",
        "read:/",
        "write:/home/user",
        "read:$HOME/$HOME/x",
        "outbound",
        "inbound",
    ];

    /// Mostly-valid permission lines, with the hostile ones mixed in.
    #[test]
    fn generated_permissions_hold_the_invariants() {
        for_seeds(
            "pkgstore::generated_permissions_hold_the_invariants",
            |_, rng| {
                let mut text = String::new();
                for _ in 0..rng.range(0, 40) {
                    let line = match rng.below(3) {
                        0 => String::from(*rng.pick(INTERFACES)),
                        1 => format!("os.lazy.gen{}.v1", rng.below(400)),
                        _ => format!("publish:t/{}/{}", rng.below(500), rng.below(500)),
                    };
                    text.push_str(if rng.one_in(3) {
                        rng.pick(PATTERNS)
                    } else {
                        &line
                    });
                    text.push('\n');
                }
                run(text.as_bytes());
            },
        );
    }

    /// Enough distinct requests that the rule budget is exceeded: the refusal
    /// must be the documented one, not a panic or a silent truncation.
    #[test]
    fn the_rule_budget_is_enforced() {
        let mut text = String::new();
        for index in 0..300 {
            text.push_str(&format!("publish:t/{index}/{index}\n"));
        }
        assert!(manifest_from_lines(&text).len() > 4000);
        run(text.as_bytes());
        let manifest = lazypkg::parse_manifest(&manifest_from_lines(&text)).unwrap();
        assert!(matches!(
            rules::compile(&manifest),
            Err(CompileError::TooManyRules { .. })
        ));
    }

    #[test]
    fn raw_noise_and_mutated_manifests_are_safe() {
        let valid = format!(
            "{HEADER}[permissions]\ninterfaces = [\"os.lazy.confd.v1\"]\n\
             topics = [\"publish:app/org.lazy.demo/x\"]\nfiles = [\"read:$HOME/*\"]\n\
             network = [\"outbound\"]\n"
        );
        for_seeds(
            "pkgstore::raw_noise_and_mutated_manifests_are_safe",
            |_, rng: &mut Rng| {
                let mut data = valid.clone().into_bytes();
                let flips = rng.range(0, 5) as usize;
                rng.flip_bits(&mut data, flips);
                run(&data);
                let len = rng.range(0, 200) as usize;
                run(&rng.bytes(len));
            },
        );
    }
}
