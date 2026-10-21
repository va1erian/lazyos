//! The syscall ABI against its frozen table (`abi/syscalls.abi`,
//! docs/update-plan.md decision 18).
//!
//! The kernel and the programs on it update as separate packages, so the
//! numbers they share may only grow. This reads the constants out of the
//! sources (`nr`, `msg::op`, `dev::op`) and holds them to the table:
//!
//! * every constant is in the table with the same number;
//! * every table entry is still a constant, unless it is marked `removed`
//!   (an entry is keyed by namespace and name: flag constants share values
//!   with ops, so a number alone does not identify one);
//! * the ABI version moves with the table: `KERNEL_ABI` is the highest
//!   `since` or `removed` in it, and a removal raises `KERNEL_ABI_COMPAT_FROM`.
//!
//! A new constant is added to the table with `since <KERNEL_ABI + 1>` and the
//! version bumped in the same change; `print_current_table` shows the lines.

use std::collections::BTreeMap;
use std::path::PathBuf;

use lazyos_sys::abi::{KERNEL_ABI, KERNEL_ABI_COMPAT_FROM};

fn crate_file(path: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(root.join(path))
        .unwrap_or_else(|e| panic!("reading {path}: {e}"))
        .replace("\r\n", "\n")
}

/// `pub const NAME: TYPE = VALUE;` lines of `source` that are plain numbers.
fn numbers(source: &str) -> BTreeMap<String, u64> {
    source
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("pub const ")?;
            let (name, rest) = rest.split_once(':')?;
            let (_, value) = rest.split_once('=')?;
            let value = value.trim().trim_end_matches(';').replace('_', "");
            let value = match value.strip_prefix("0x") {
                Some(hex) => u64::from_str_radix(hex, 16).ok()?,
                None => value.parse().ok()?,
            };
            Some((name.trim().to_string(), value))
        })
        .collect()
}

/// The text of `pub mod op { ... }` in `source`.
fn op_module(source: &str) -> &str {
    let start = source.find("pub mod op {").expect("pub mod op");
    let body = &source[start..];
    let mut depth = 0;
    for (at, ch) in body.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &body[..at];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated pub mod op");
}

/// Namespace, name and number of every constant that is part of the ABI.
fn current() -> Vec<(String, String, u64)> {
    let sources = [
        ("native", numbers(&crate_file("src/nr.rs"))),
        (
            "messenger",
            numbers(op_module(&crate_file("src/msg/abi.rs"))),
        ),
        ("device", numbers(op_module(&crate_file("src/dev.rs")))),
    ];
    let mut out = Vec::new();
    for (space, table) in sources {
        out.extend(
            table
                .into_iter()
                .map(|(name, n)| (space.to_string(), name, n)),
        );
    }
    out.sort();
    out
}

struct Entry {
    number: u64,
    since: u32,
    removed: Option<u32>,
}

/// `namespace number NAME since N [removed M]` lines; `#` starts a comment.
fn frozen() -> BTreeMap<(String, String), Entry> {
    let mut table = BTreeMap::new();
    for line in crate_file("abi/syscalls.abi").lines() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        let ok = (f.len() == 5 || f.len() == 7)
            && f[3] == "since"
            && (f.len() == 5 || f[5] == "removed");
        assert!(ok, "bad line in abi/syscalls.abi: `{line}`");
        let entry = Entry {
            number: f[1].parse().expect("number"),
            since: f[4].parse().expect("since"),
            removed: f.get(6).map(|r| r.parse().expect("removed")),
        };
        let key = (f[0].to_string(), f[2].to_string());
        assert!(
            table.insert(key, entry).is_none(),
            "duplicate entry `{line}`"
        );
    }
    table
}

#[test]
fn every_constant_is_in_the_frozen_table_unchanged() {
    let frozen = frozen();
    for (space, name, number) in current() {
        let entry = frozen.get(&(space.clone(), name.clone())).unwrap_or_else(|| {
            panic!(
                "{space} {number} {name} is not in abi/syscalls.abi: add `{space} {number} {name} since {}` and bump KERNEL_ABI",
                KERNEL_ABI + 1
            )
        });
        assert_eq!(
            entry.number, number,
            "{space} {name} was renumbered (a renumbering is a break)"
        );
        assert!(
            entry.removed.is_none(),
            "{space} {name} is marked removed but still exists"
        );
    }
}

#[test]
fn no_frozen_entry_disappears_without_a_removed_mark() {
    let now: Vec<(String, String)> = current()
        .into_iter()
        .map(|(s, name, _)| (s, name))
        .collect();
    for ((space, name), entry) in frozen() {
        if entry.removed.is_none() {
            assert!(
                now.contains(&(space.clone(), name.clone())),
                "{space} {} {name} was removed or renamed",
                entry.number
            );
        }
    }
}

#[test]
fn the_abi_version_moves_with_the_table() {
    let frozen = frozen();
    let newest = frozen
        .values()
        .flat_map(|e| [e.since, e.removed.unwrap_or(0)])
        .max()
        .unwrap();
    assert_eq!(
        KERNEL_ABI, newest,
        "KERNEL_ABI must equal the newest `since`/`removed` in the table"
    );
    for ((space, name), entry) in &frozen {
        assert!(
            entry.since >= 1 && entry.since <= KERNEL_ABI,
            "{space} {name}: since {}",
            entry.since
        );
        if let Some(removed) = entry.removed {
            assert!(
                removed > entry.since,
                "{space} {name}: removed before it existed"
            );
            assert!(
                KERNEL_ABI_COMPAT_FROM >= removed,
                "{space} {name} was removed in ABI {removed}: raise KERNEL_ABI_COMPAT_FROM"
            );
        }
    }
    assert!((1..=KERNEL_ABI).contains(&KERNEL_ABI_COMPAT_FROM));
}

/// `cargo test -p lazyos-sys --test abi_freeze -- --ignored --nocapture`
#[test]
#[ignore = "prints the table lines for a new ABI"]
fn print_current_table() {
    for (space, name, number) in current() {
        println!("{space} {number} {name} since {}", KERNEL_ABI);
    }
}
