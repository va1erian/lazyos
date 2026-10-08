#![forbid(unsafe_code)]

//! Unit tests for the window's pure text helpers.

use std::ffi::OsString;

use super::actions::delete_prompt;
use crate::model::Entry;
use crate::platform::{Kind, Meta, RawEntry};

fn entry(name: &str, kind: Kind) -> Entry {
    Entry::from_raw(RawEntry {
        name: OsString::from(name),
        meta: Meta::bare(std::path::Path::new(name), kind),
    })
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
fn a_paste_summary_counts_reports_failures_and_an_empty_clipboard() {
    use super::clipboard::paste_summary;
    use crate::platform::Pasted;
    use std::path::PathBuf;

    let done = |copied, failed| Ok(Pasted { copied, failed });
    assert_eq!(paste_summary(&done(1, Vec::new())), "Pasted 1 item");
    assert_eq!(paste_summary(&done(3, Vec::new())), "Pasted 3 items");
    assert!(paste_summary(&done(0, Vec::new())).contains("Nothing to paste"));
    let failed = vec![(PathBuf::from("/a/gone.txt"), "not found".to_string())];
    let text = paste_summary(&done(2, failed));
    assert!(
        text.contains("Pasted 2 items") && text.contains("gone.txt: not found"),
        "{text}"
    );
    let error = std::io::Error::other("no service");
    assert!(paste_summary(&Err(error)).starts_with("Cannot paste"));
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
