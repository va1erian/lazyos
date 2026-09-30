#![forbid(unsafe_code)]

//! Unit tests for the window's pure text helpers.

use std::ffi::OsString;

use super::actions::delete_prompt;
use crate::model::Entry;
use crate::platform::Kind;

fn entry(name: &str, kind: Kind) -> Entry {
    Entry {
        name: OsString::from(name),
        display: name.to_string(),
        detail: match kind {
            Kind::Dir => "Folder".to_string(),
            _ => "File".to_string(),
        },
        kind,
        size: None,
    }
}

#[test]
fn the_prompt_names_one_file() {
    let prompt = delete_prompt(&[entry("notes.txt", Kind::File)]);
    assert!(prompt.contains("notes.txt"), "{prompt}");
    assert!(prompt.contains("cannot be undone"), "{prompt}");
}

#[test]
fn the_prompt_warns_that_a_folder_takes_its_contents() {
    let prompt = delete_prompt(&[entry("docs", Kind::Dir)]);
    assert!(prompt.contains("docs"), "{prompt}");
    assert!(prompt.contains("everything inside"), "{prompt}");
}

#[test]
fn the_prompt_counts_several_items() {
    let prompt = delete_prompt(&[
        entry("a", Kind::File),
        entry("b", Kind::Dir),
        entry("c", Kind::File),
    ]);
    assert!(prompt.contains('3'), "{prompt}");
    assert!(prompt.contains("everything inside"), "{prompt}");
}
