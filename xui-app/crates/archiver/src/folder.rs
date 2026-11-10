//! The folder view: one archive folder's rows, built from the flat entry
//! list, with the folders an archive only implies (`a/b/c.txt` and no `a/`
//! entry) synthesised, folder sizes summed, and 7-Zip's order (`..` first,
//! then folders, then files, each sorted by the chosen column).

use std::cmp::Ordering;
use std::collections::HashMap;

use lazyarc::{Entry, EntryKind};
use xui_core::natural_cmp;

/// What a row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// The `..` row that goes up a folder.
    Parent,
    Folder,
    File,
    Link,
}

/// One row of the folder view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub kind: RowKind,
    /// The full archive path (`""` for the parent row).
    pub path: String,
    /// Bytes: a file's size, a folder's total.
    pub size: u64,
    /// Compressed bytes, when every file below records them.
    pub packed: Option<u64>,
    pub modified: Option<i64>,
    pub method: String,
    /// Files below a folder (1 for a file).
    pub files: u64,
    pub encrypted: bool,
}

/// The columns, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Name,
    Size,
    Packed,
    Modified,
    Method,
}

impl Column {
    pub const ALL: [Column; 5] = [
        Column::Name,
        Column::Size,
        Column::Packed,
        Column::Modified,
        Column::Method,
    ];

    pub fn from_index(index: usize) -> Column {
        Column::ALL.get(index).copied().unwrap_or(Column::Name)
    }

    pub fn title(self) -> &'static str {
        match self {
            Column::Name => "Name",
            Column::Size => "Size",
            Column::Packed => "Packed",
            Column::Modified => "Modified",
            Column::Method => "Method",
        }
    }
}

/// The rows of `folder` (a normalised archive path, `""` for the root).
pub fn rows(entries: &[Entry], folder: &str) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    let prefix_len = if folder.is_empty() {
        0
    } else {
        folder.len() + 1
    };
    for entry in entries {
        if entry.path == folder || !entry.is_under(folder) {
            continue;
        }
        let rest = &entry.path[prefix_len..];
        let (name, deeper) = match rest.split_once('/') {
            Some((name, _)) => (name, true),
            None => (rest, false),
        };
        let index = *by_name.entry(name.to_owned()).or_insert_with(|| {
            rows.push(Row {
                name: name.to_owned(),
                kind: RowKind::Folder,
                path: if folder.is_empty() {
                    name.to_owned()
                } else {
                    format!("{folder}/{name}")
                },
                size: 0,
                packed: Some(0),
                modified: None,
                method: String::new(),
                files: 0,
                encrypted: false,
            });
            rows.len() - 1
        });
        let row = &mut rows[index];
        if deeper || entry.kind.is_dir() {
            // A folder, explicit or implied: sum what lies below it.
            if !deeper {
                row.modified = entry.modified.or(row.modified);
            }
            if matches!(entry.kind, EntryKind::File) {
                add_file(row, entry);
            }
            continue;
        }
        row.kind = if matches!(entry.kind, EntryKind::Symlink { .. }) {
            RowKind::Link
        } else {
            RowKind::File
        };
        row.size = entry.size;
        row.packed = entry.packed;
        row.modified = entry.modified;
        row.method = entry.method.clone();
        row.files = 1;
        row.encrypted = entry.encrypted;
    }
    rows
}

fn add_file(row: &mut Row, entry: &Entry) {
    row.size += entry.size;
    row.files += 1;
    row.packed = match (row.packed, entry.packed) {
        (Some(sum), Some(packed)) => Some(sum + packed),
        _ => None,
    };
    row.encrypted |= entry.encrypted;
}

/// The parent row shown above a folder's rows.
pub fn parent_row() -> Row {
    Row {
        name: "..".to_owned(),
        kind: RowKind::Parent,
        path: String::new(),
        size: 0,
        packed: None,
        modified: None,
        method: String::new(),
        files: 0,
        encrypted: false,
    }
}

/// Sort `rows` by `column`: the parent row first, folders before files.
pub fn sort(rows: &mut [Row], column: Column, ascending: bool) {
    rows.sort_by(|a, b| compare(a, b, column, ascending));
}

/// Sort `rows` like [`sort`] and return where the rows in `selection` went,
/// so a header click keeps the user's selection on the same items.
pub fn sort_keeping(
    rows: &mut Vec<Row>,
    selection: &[usize],
    column: Column,
    ascending: bool,
) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| compare(&rows[a], &rows[b], column, ascending));
    let mut position = vec![0; rows.len()];
    for (new, &old) in order.iter().enumerate() {
        position[old] = new;
    }
    let mut old: Vec<Option<Row>> = std::mem::take(rows).into_iter().map(Some).collect();
    rows.extend(order.iter().filter_map(|&i| old[i].take()));
    let mut kept: Vec<usize> = selection
        .iter()
        .filter_map(|&row| position.get(row).copied())
        .collect();
    kept.sort_unstable();
    kept.dedup();
    kept
}

/// The order of two rows: `..` first, then folders, then files, each group
/// by `column` with the name (natural order) as the tie-break.
fn compare(a: &Row, b: &Row, column: Column, ascending: bool) -> Ordering {
    let group = |row: &Row| match row.kind {
        RowKind::Parent => 0,
        RowKind::Folder => 1,
        _ => 2,
    };
    group(a).cmp(&group(b)).then_with(|| {
        let order = match column {
            Column::Name => natural_cmp(&a.name, &b.name),
            Column::Size => a.size.cmp(&b.size),
            Column::Packed => a.packed.cmp(&b.packed),
            Column::Modified => a.modified.cmp(&b.modified),
            Column::Method => natural_cmp(&a.method, &b.method),
        }
        .then_with(|| natural_cmp(&a.name, &b.name));
        if ascending {
            order
        } else {
            order.reverse()
        }
    })
}

/// The parent of normalised `folder` (`a/b` -> `a`, `a` -> `""`).
pub fn parent(folder: &str) -> String {
    folder
        .rsplit_once('/')
        .map(|(up, _)| up.to_owned())
        .unwrap_or_default()
}

/// Whether `folder` still exists in `entries` (explicitly or implied).
pub fn exists(entries: &[Entry], folder: &str) -> bool {
    folder.is_empty()
        || entries
            .iter()
            .any(|e| e.path != folder && e.is_under(folder) || e.path == folder && e.kind.is_dir())
}

/// The archive paths the selected `rows` stand for (the parent row stands
/// for nothing).
pub fn selected_paths(rows: &[Row], selection: &[usize]) -> Vec<String> {
    selection
        .iter()
        .filter_map(|&i| rows.get(i))
        .filter(|row| row.kind != RowKind::Parent)
        .map(|row| row.path.clone())
        .collect()
}

/// `count` rows of `bytes` as the status bar says it.
pub fn summary(rows: &[Row], selection: &[usize]) -> String {
    let real: Vec<&Row> = rows.iter().filter(|r| r.kind != RowKind::Parent).collect();
    let items = format!(
        "{} item{}",
        real.len(),
        if real.len() == 1 { "" } else { "s" }
    );
    let chosen: Vec<&Row> = selection
        .iter()
        .filter_map(|&i| rows.get(i))
        .filter(|r| r.kind != RowKind::Parent)
        .collect();
    if chosen.is_empty() {
        return items;
    }
    let bytes: u64 = chosen.iter().map(|r| r.size).sum();
    format!(
        "{items}, {} selected ({})",
        chosen.len(),
        crate::cells::size(bytes)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(index: usize, path: &str, size: u64) -> Entry {
        let mut entry = Entry::new(index, path, EntryKind::File);
        entry.size = size;
        entry.packed = Some(size / 2);
        entry
    }

    fn dir(index: usize, path: &str) -> Entry {
        Entry::new(index, path, EntryKind::Dir)
    }

    fn sample() -> Vec<Entry> {
        vec![
            file(0, "readme.txt", 10),
            file(1, "src/main.rs", 100),
            file(2, "src/lib/a.rs", 40),
            dir(3, "docs"),
            file(4, "src/lib/b.rs", 60),
        ]
    }

    #[test]
    fn implied_folders_appear_with_summed_sizes() {
        let rows = rows(&sample(), "");
        let src = rows.iter().find(|r| r.name == "src").unwrap();
        assert_eq!(
            (src.kind, src.size, src.files, src.packed),
            (RowKind::Folder, 200, 3, Some(100))
        );
        let docs = rows.iter().find(|r| r.name == "docs").unwrap();
        assert_eq!((docs.kind, docs.files), (RowKind::Folder, 0));
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn a_subfolder_lists_only_its_children() {
        let rows = rows(&sample(), "src");
        let names: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["main.rs", "lib"]);
        assert_eq!(rows[1].path, "src/lib");
    }

    #[test]
    fn folders_sort_first_and_numbers_naturally() {
        let mut rows = rows(&sample(), "");
        rows.insert(0, parent_row());
        sort(&mut rows, Column::Name, false);
        let names: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["..", "src", "docs", "readme.txt"]);
    }

    #[test]
    fn sorting_keeps_the_selection_on_its_items() {
        let mut rows: Vec<Row> = ["file10", "File2", "file1"]
            .iter()
            .map(|name| Row {
                name: (*name).to_owned(),
                kind: RowKind::File,
                path: (*name).to_owned(),
                ..parent_row()
            })
            .collect();
        rows.insert(0, parent_row());
        // Select `File2` (row 2) and `file1` (row 3).
        let kept = sort_keeping(&mut rows, &[2, 3], Column::Name, true);
        let names: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["..", "file1", "File2", "file10"]);
        assert_eq!(kept, [1, 2]);
        let kept = sort_keeping(&mut rows, &kept, Column::Name, false);
        let names: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["..", "file10", "File2", "file1"]);
        assert_eq!(kept, [2, 3]);
        assert!(sort_keeping(&mut rows, &[99], Column::Name, true).is_empty());
    }

    #[test]
    fn parents_and_existence() {
        assert_eq!(parent("a/b/c"), "a/b");
        assert_eq!(parent("a"), "");
        let entries = sample();
        assert!(exists(&entries, "src/lib"));
        assert!(exists(&entries, "docs"));
        assert!(!exists(&entries, "readme.txt"));
        assert!(!exists(&entries, "nope"));
    }

    #[test]
    fn the_parent_row_is_never_selected_content() {
        let mut list = rows(&sample(), "src");
        list.insert(0, parent_row());
        assert_eq!(selected_paths(&list, &[0, 1]), ["src/main.rs"]);
        assert_eq!(summary(&list, &[0, 1]), "2 items, 1 selected (100 B)");
    }
}
