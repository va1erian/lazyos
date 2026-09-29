//! `spawn_line::argv`: whitespace splitting with double-quoted tokens, the
//! mechanism `init` uses to pass one file path (possibly with spaces) as a
//! single `argv` item.

use super::*;
use crate::process::spawn_line::argv;

fn items(args: &str) -> Option<Vec<String>> {
    argv(args)
}

fn want(list: &[&str]) -> Option<Vec<String>> {
    Some(list.iter().map(|item| String::from(*item)).collect())
}

/// Plain, quoted, empty and malformed argument strings.
pub fn argv_splitting_rules() -> Result<(), String> {
    check!(items("") == want(&[]), "empty: {:?}", items(""));
    check!(items("   ") == want(&[]), "blank: {:?}", items("   "));
    check!(
        items("--client attempt=1") == want(&["--client", "attempt=1"]),
        "plain: {:?}",
        items("--client attempt=1")
    );
    check!(
        items("--client \"/home/me/My Notes.txt\" attempt=2")
            == want(&["--client", "/home/me/My Notes.txt", "attempt=2"]),
        "quoted: {:?}",
        items("--client \"/home/me/My Notes.txt\" attempt=2")
    );
    check!(
        items("\"\"") == want(&[""]),
        "empty quotes: {:?}",
        items("\"\"")
    );
    check!(items("\"a") == None, "unterminated quote accepted");
    check!(items("\"a\"b") == None, "text after closing quote accepted");
    check!(
        items("a\"b c\"") == want(&["a\"b", "c\""]),
        "a mid-token quote is literal: {:?}",
        items("a\"b c\"")
    );
    Ok(())
}

/// Many generated lines round-trip: every path (with and without spaces)
/// comes back as exactly one item, never split and never merged.
pub fn argv_roundtrip_soak() -> Result<(), String> {
    for index in 0..5_000u32 {
        let name = if index % 3 == 0 {
            alloc::format!("/home/user/dir {index}/file name {index}.txt")
        } else {
            alloc::format!("/data/f{index}.txt")
        };
        let line = if name.contains(' ') {
            alloc::format!("--client \"{name}\" attempt={index}")
        } else {
            alloc::format!("--client {name} attempt={index}")
        };
        let got = items(&line);
        let expected = alloc::format!("attempt={index}");
        check!(
            got == want(&["--client", &name, &expected]),
            "line {line:?} parsed as {got:?}"
        );
    }
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("spawn_argv_splitting_rules", argv_splitting_rules),
    ("spawn_argv_roundtrip_soak", argv_roundtrip_soak),
];
