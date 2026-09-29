#![forbid(unsafe_code)]

//! The status bar's strings, produced purely from a listing and a selection.

use super::Entry;
use super::format_size;
use crate::platform::Kind;

/// Builds the status bar's parts for `entries` and `selection` (indices into
/// `entries`):
///
/// 1. the item count with its folder/file breakdown, e.g.
///    `"12 items (3 folders, 9 files)"`;
/// 2. the total size of the files directly inside;
/// 3. when something is selected, `"3 selected (2.0 KiB)"`, or the item's own
///    name and detail for a single selection.
///
/// The third part is absent when the selection is empty. Out-of-range selection
/// indices are ignored.
pub fn summarize(entries: &[Entry], selection: &[usize]) -> Vec<String> {
    let folders = entries
        .iter()
        .filter(|entry| entry.kind == Kind::Dir)
        .count();
    let files = entries.len() - folders;

    let mut parts = vec![
        count_part(entries.len(), folders, files),
        format_size(total_size(entries.iter())),
    ];
    if let Some(line) = selection_part(entries, selection) {
        parts.push(line);
    }
    parts
}

/// `"12 items (3 folders, 9 files)"`, dropping a zero clause and using the
/// singular where it fits.
fn count_part(total: usize, folders: usize, files: usize) -> String {
    if total == 0 {
        return "0 items".to_string();
    }
    let mut clauses = Vec::new();
    if folders > 0 {
        clauses.push(format!("{folders} {}", plural(folders, "folder")));
    }
    if files > 0 {
        clauses.push(format!("{files} {}", plural(files, "file")));
    }
    format!("{total} {} ({})", plural(total, "item"), clauses.join(", "))
}

/// `"3 selected (2.0 KiB)"`, or `"Documents (Folder)"` for one item. A
/// selection of folders only drops the size clause.
fn selection_part(entries: &[Entry], selection: &[usize]) -> Option<String> {
    let selected: Vec<&Entry> = selection
        .iter()
        .filter_map(|index| entries.get(*index))
        .collect();
    match selected.as_slice() {
        [] => None,
        [entry] => Some(format!("{} ({})", entry.display, entry.detail)),
        many => {
            let has_file = many.iter().any(|entry| entry.kind != Kind::Dir);
            if has_file {
                Some(format!(
                    "{} selected ({})",
                    many.len(),
                    format_size(total_size(many.iter().copied()))
                ))
            } else {
                Some(format!("{} selected", many.len()))
            }
        }
    }
}

/// The summed size of the files (and symlinks) among `entries`.
fn total_size<'a>(entries: impl Iterator<Item = &'a Entry>) -> u64 {
    entries
        .filter(|entry| entry.kind != Kind::Dir)
        .filter_map(|entry| entry.size)
        .sum()
}

/// `"folder"` or `"folders"`.
fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}
