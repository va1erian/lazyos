#![forbid(unsafe_code)]

//! The listing order: folders first, then files, each group ordered by the
//! window's [`SortOrder`] (name, size, type or modification time, ascending
//! or descending), with the case-folded name as the tie-break.

use std::cmp::Ordering;

use super::Entry;
use crate::platform::Kind;

/// What a listing is sorted by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortKey {
    /// The name, case-insensitively.
    #[default]
    Name,
    /// A file's size (folders have none and keep name order).
    Size,
    /// The type shown in the details view (`"PNG File"`, `"Folder"`).
    Type,
    /// The last modification time; an unknown time sorts first.
    Modified,
}

impl SortKey {
    /// Every key, in menu and details-column order.
    pub const ALL: [SortKey; 4] = [
        SortKey::Name,
        SortKey::Size,
        SortKey::Type,
        SortKey::Modified,
    ];

    /// The key's label, as the sort menu and the details header show it.
    pub const fn label(self) -> &'static str {
        match self {
            SortKey::Name => "Name",
            SortKey::Size => "Size",
            SortKey::Type => "Type",
            SortKey::Modified => "Modified",
        }
    }

    /// The details view column this key sorts (the inverse of [`of_column`](Self::of_column)).
    pub fn column(self) -> usize {
        SortKey::ALL
            .iter()
            .position(|key| *key == self)
            .unwrap_or(0)
    }

    /// The key a details view column sorts by.
    pub fn of_column(column: usize) -> Option<SortKey> {
        SortKey::ALL.get(column).copied()
    }
}

/// A key and a direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SortOrder {
    /// What to compare.
    pub key: SortKey,
    /// Largest (or last) first.
    pub descending: bool,
}

impl SortOrder {
    /// Sorting by `key`: the same key flips the direction (a second click on a
    /// column header), a new key starts ascending.
    pub fn toggled(self, key: SortKey) -> SortOrder {
        if key == self.key {
            SortOrder {
                key,
                descending: !self.descending,
            }
        } else {
            SortOrder {
                key,
                descending: false,
            }
        }
    }
}

/// Sorts entries in place: directories first in every order, then everything
/// else, each group ordered by `order` with a case-folded name and then a
/// byte-order tie-break, so equal keys have a deterministic order.
///
/// Works on non-UTF-8 names too: the display string is lossy, so an invalid
/// name never panics.
pub fn sort_entries(entries: &mut [Entry], order: SortOrder) {
    entries.sort_by(|a, b| {
        group(a)
            .cmp(&group(b))
            .then_with(|| {
                let by_key = compare(a, b, order.key);
                if order.descending {
                    by_key.reverse()
                } else {
                    by_key
                }
            })
            .then_with(|| by_name(a, b))
    });
}

/// The ordering of `a` and `b` by `key` alone (ties are left to the caller).
fn compare(a: &Entry, b: &Entry, key: SortKey) -> Ordering {
    match key {
        SortKey::Name => by_name(a, b),
        SortKey::Size => a.size.cmp(&b.size),
        SortKey::Type => a.type_name.to_lowercase().cmp(&b.type_name.to_lowercase()),
        SortKey::Modified => a.modified.cmp(&b.modified),
    }
}

/// Case-insensitive name order, then the exact display and raw name.
fn by_name(a: &Entry, b: &Entry) -> Ordering {
    a.display
        .to_lowercase()
        .cmp(&b.display.to_lowercase())
        .then_with(|| a.display.cmp(&b.display))
        .then_with(|| a.name.cmp(&b.name))
}

/// The folder/file grouping. Directories sort before files.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    Folder,
    File,
}

fn group(entry: &Entry) -> Group {
    if entry.kind == Kind::Dir {
        Group::Folder
    } else {
        Group::File
    }
}
