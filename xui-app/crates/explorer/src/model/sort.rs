#![forbid(unsafe_code)]

//! The fixed listing order: folders first, then files, each group sorted
//! case-insensitively by name.

use super::Entry;
use crate::platform::Kind;

/// Sorts entries in place: directories first, then everything else, each group
/// ordered by a case-folded name with a stable, byte-order tie-break so names
/// that differ only in case have a deterministic order.
///
/// Works on non-UTF-8 names too: the display string is lossy, so an invalid
/// name never panics.
pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by_cached_key(|entry| {
        let group = if entry.kind == Kind::Dir {
            Group::Folder
        } else {
            Group::File
        };
        (
            group,
            entry.display.to_lowercase(),
            entry.display.clone(),
            entry.name.clone(),
        )
    });
}

/// The folder/file grouping. Directories sort before files.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    Folder,
    File,
}
