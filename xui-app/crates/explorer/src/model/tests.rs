#![forbid(unsafe_code)]

//! Pure unit tests for the model: order, summaries, properties, formatting and
//! path helpers. None touches the disk or a widget.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use super::*;
use crate::platform::{Kind, Meta, RawEntry};

fn entry(name: &str, kind: Kind, size: Option<u64>) -> Entry {
    entry_at(name, kind, size, None)
}

fn entry_at(name: &str, kind: Kind, size: Option<u64>, modified: Option<u64>) -> Entry {
    let mut meta = Meta::bare(&Path::new("/x").join(name), kind);
    meta.size = size;
    meta.modified = modified.map(at);
    Entry::from_raw(RawEntry {
        name: OsString::from(name),
        meta,
    })
}

/// A listing of `entries` in the order given.
fn listing(entries: Vec<Entry>) -> Listing {
    Listing {
        dir: PathBuf::from("/a"),
        entries,
        error: None,
    }
}

fn names(entries: &[Entry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.display.as_str()).collect()
}

fn meta(name: &str, parent: &str, kind: Kind, size: Option<u64>, entries: Option<usize>) -> Meta {
    Meta {
        name: OsString::from(name),
        parent: Some(PathBuf::from(parent)),
        kind,
        size,
        modified: None,
        created: None,
        readonly: false,
        entries,
    }
}

#[test]
fn folders_sort_before_files_case_insensitively() {
    let mut entries = vec![
        entry("zeta.txt", Kind::File, Some(1)),
        entry("Alpha", Kind::Dir, None),
        entry("beta.log", Kind::File, Some(1)),
        entry("apple", Kind::Dir, None),
    ];
    sort_entries(&mut entries, SortOrder::default());
    assert_eq!(names(&entries), ["Alpha", "apple", "beta.log", "zeta.txt"]);
}

#[test]
fn every_key_sorts_both_ways_with_folders_first() {
    let entries = vec![
        entry_at("b.txt", Kind::File, Some(30), Some(100)),
        entry_at("a.png", Kind::File, Some(10), Some(300)),
        entry_at("c.md", Kind::File, Some(20), None),
        entry_at("zdir", Kind::Dir, None, Some(50)),
        entry_at("adir", Kind::Dir, None, Some(60)),
    ];
    let sorted = |key, descending| {
        let mut entries = entries.clone();
        sort_entries(&mut entries, SortOrder { key, descending });
        names(&entries)
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        sorted(SortKey::Name, false),
        ["adir", "zdir", "a.png", "b.txt", "c.md"]
    );
    assert_eq!(
        sorted(SortKey::Name, true),
        ["zdir", "adir", "c.md", "b.txt", "a.png"]
    );
    assert_eq!(
        sorted(SortKey::Size, false),
        ["adir", "zdir", "a.png", "c.md", "b.txt"]
    );
    assert_eq!(
        sorted(SortKey::Size, true),
        ["adir", "zdir", "b.txt", "c.md", "a.png"]
    );
    // MD File < PNG File < TXT File.
    assert_eq!(
        sorted(SortKey::Type, false),
        ["adir", "zdir", "c.md", "a.png", "b.txt"]
    );
    // An unknown time sorts first ascending.
    assert_eq!(
        sorted(SortKey::Modified, false),
        ["zdir", "adir", "c.md", "b.txt", "a.png"]
    );
    assert_eq!(
        sorted(SortKey::Modified, true),
        ["adir", "zdir", "a.png", "b.txt", "c.md"]
    );
}

#[test]
fn a_header_click_flips_the_same_key_and_starts_a_new_one_ascending() {
    let order = SortOrder::default();
    let flipped = order.toggled(SortKey::Name);
    assert!(flipped.descending);
    let by_size = flipped.toggled(SortKey::Size);
    assert_eq!(
        by_size,
        SortOrder {
            key: SortKey::Size,
            descending: false
        }
    );
    for key in SortKey::ALL {
        assert_eq!(SortKey::of_column(key.column()), Some(key));
    }
    assert_eq!(SortKey::of_column(9), None);
}

#[test]
fn the_type_column_names_folders_links_and_extensions() {
    use std::ffi::OsStr;
    assert_eq!(type_name(Kind::Dir, OsStr::new("a.png")), "Folder");
    assert_eq!(type_name(Kind::Symlink, OsStr::new("a.png")), "Link");
    assert_eq!(type_name(Kind::File, OsStr::new("a.png")), "PNG File");
    assert_eq!(type_name(Kind::File, OsStr::new("Makefile")), "File");
    assert_eq!(type_name(Kind::File, OsStr::new(".bashrc")), "File");
}

#[test]
fn the_details_cells_follow_the_columns() {
    use std::rc::Rc;
    use xui_core::widget::ListModel;

    let model = SharedListing::new(Rc::new(listing(vec![
        entry("docs", Kind::Dir, None),
        entry_at("notes.txt", Kind::File, Some(2048), Some(0)),
    ])));
    assert_eq!(model.rows(), 2);
    assert_eq!(model.cell(0, 0), Some("docs"));
    assert_eq!(model.cell(0, 1), Some(""), "a folder has no size");
    assert_eq!(model.cell(0, 2), Some("Folder"));
    assert_eq!(model.cell(1, 1), Some("2.0 KiB"));
    assert_eq!(model.cell(1, 2), Some("TXT File"));
    assert_eq!(model.cell(1, 3), Some("1970-01-01 00:00"));
    assert_eq!(model.cell(1, 4), None);
}

#[test]
fn a_non_utf8_name_sorts_without_panicking() {
    let raw = |name: OsString| {
        Entry::from_raw(RawEntry {
            meta: Meta::bare(Path::new(&name), Kind::File),
            name,
        })
    };
    let mut entries = vec![raw(OsString::from("z"))];
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        entries.push(raw(OsString::from_vec(vec![0xff, 0x61])));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        entries.push(raw(OsString::from_wide(&[0xD800, 0x0061])));
    }
    sort_entries(&mut entries, SortOrder::default());
    assert_eq!(entries.len(), 2);
}

#[test]
fn summarize_names_the_breakdown_and_total_size() {
    let entries = vec![
        entry("docs", Kind::Dir, None),
        entry("a.txt", Kind::File, Some(1024)),
        entry("b.txt", Kind::File, Some(24)),
    ];
    let parts = summarize(&entries, &[]);
    assert_eq!(parts[0], "3 items (1 folder, 2 files)");
    assert_eq!(parts[1], "1.0 KiB");
    assert_eq!(parts.len(), 2, "no selection yields two parts");
}

#[test]
fn summarize_of_an_empty_folder() {
    assert_eq!(summarize(&[], &[]), vec!["0 items", "0 B"]);
}

#[test]
fn summarize_of_folders_only_has_no_size_clause() {
    let entries = vec![entry("a", Kind::Dir, None), entry("b", Kind::Dir, None)];
    let parts = summarize(&entries, &[0, 1]);
    assert_eq!(parts[0], "2 items (2 folders)");
    assert_eq!(parts[2], "2 selected");
}

#[test]
fn summarize_describes_one_selected_item() {
    let entries = vec![
        entry("docs", Kind::Dir, None),
        entry("report.txt", Kind::File, Some(2048)),
    ];
    let one_file = summarize(&entries, &[1]);
    assert_eq!(one_file[2], "report.txt (2.0 KiB)");
    let one_dir = summarize(&entries, &[0]);
    assert_eq!(one_dir[2], "docs (Folder)");
}

#[test]
fn summarize_counts_a_multi_selection_size() {
    let entries = vec![
        entry("a.txt", Kind::File, Some(1024)),
        entry("b.txt", Kind::File, Some(1024)),
        entry("c.txt", Kind::File, Some(1024)),
    ];
    let parts = summarize(&entries, &[0, 1, 2]);
    assert_eq!(parts[2], "3 selected (3.0 KiB)");
}

#[test]
fn format_size_covers_the_boundaries() {
    assert_eq!(format_size(0), "0 B");
    assert_eq!(format_size(1023), "1023 B");
    assert_eq!(format_size(1024), "1.0 KiB");
    assert_eq!(format_size(1536), "1.5 KiB");
    assert_eq!(format_size(1024 * 1024), "1.0 MiB");
    assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GiB");
}

fn at(seconds: u64) -> std::time::SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

#[test]
fn format_time_is_utc_civil() {
    assert_eq!(format_time(UNIX_EPOCH), "1970-01-01 00:00");

    // 2000-02-29 00:00 UTC, a leap day.
    let leap = 11_016u64 * 86_400;
    assert_eq!(format_time(at(leap)), "2000-02-29 00:00");

    // 2100 is not a leap year: 2100-02-28 is followed by 2100-03-01.
    let feb28 = 47_540u64 * 86_400;
    assert_eq!(format_time(at(feb28)), "2100-02-28 00:00");
    assert_eq!(format_time(at(feb28 + 86_400)), "2100-03-01 00:00");

    // A time with hours and minutes.
    let noonish = UNIX_EPOCH + Duration::from_secs(3_661 * 24 * 3_600 + 13 * 3_600 + 5 * 60);
    assert_eq!(format_time(noonish), "1980-01-10 13:05");
}

#[test]
fn describe_builds_every_row() {
    let file = Meta {
        modified: Some(at(0)),
        created: Some(at(86_400)),
        readonly: true,
        size: Some(2048),
        ..meta("report.txt", "/home/user", Kind::File, Some(2048), None)
    };
    let rows = describe(&file);
    let get = |label: &str| {
        rows.iter()
            .find(|(key, _)| key == label)
            .map(|(_, value)| value.clone())
            .unwrap()
    };
    assert_eq!(get("Name"), "report.txt");
    assert_eq!(get("Kind"), "File");
    assert_eq!(get("Location"), "/home/user");
    assert_eq!(get("Size"), "2.0 KiB");
    assert_eq!(get("Modified (UTC)"), "1970-01-01 00:00");
    assert_eq!(get("Created (UTC)"), "1970-01-02 00:00");
    assert_eq!(get("Read-only"), "Yes");
}

#[test]
fn describe_a_folder_counts_entries_not_recursive_size() {
    let dir = meta("docs", "/root", Kind::Dir, None, Some(3));
    let size = describe(&dir)
        .into_iter()
        .find(|(key, _)| key == "Size")
        .map(|(_, value)| value)
        .unwrap();
    assert_eq!(size, "3 items");
    assert_eq!(
        describe(&meta("docs", "/root", Kind::Dir, None, None))[3].1,
        "—"
    );
}

#[test]
fn describe_marks_unknown_times_and_root_location() {
    let mut root = meta("", "/ignored", Kind::Dir, None, Some(0));
    root.name = OsString::new();
    root.parent = None;
    let rows = describe(&root);
    assert_eq!(rows[0].1, "—", "no name");
    assert_eq!(rows[2].1, "—", "no parent for a root");
    assert_eq!(rows[4].1, "—", "no modified time");
}

#[test]
fn title_uses_the_last_component_or_a_root_display() {
    assert_eq!(title(Path::new("/home/user/Documents")), "Documents");
    assert_eq!(title(Path::new("/home/user/Documents/")), "Documents");
    assert_eq!(title(Path::new("/")), "/");
    #[cfg(windows)]
    assert_eq!(title(Path::new("C:\\Users\\me")), "me");
}

#[test]
fn is_root_only_for_paths_without_a_parent() {
    assert!(!is_root(Path::new("/home")));
    #[cfg(windows)]
    assert!(is_root(Path::new("C:\\")));
    assert!(is_root(Path::new("/")));
}

#[test]
fn unicode_names_sort_case_insensitively() {
    let mut entries = vec![
        entry("Zebra", Kind::File, Some(1)),
        entry("ä", Kind::File, Some(1)),
        entry("Ä", Kind::File, Some(1)),
    ];
    sort_entries(&mut entries, SortOrder::default());
    let names: Vec<&str> = entries.iter().map(|entry| entry.display.as_str()).collect();
    assert_eq!(names, ["Zebra", "Ä", "ä"]);
}

#[test]
fn a_very_long_name_is_kept_verbatim() {
    let name = "a".repeat(1000);
    let entries = vec![entry(&name, Kind::File, Some(1))];
    let parts = summarize(&entries, &[0]);
    assert!(parts[0].starts_with("1 item"), "{}", parts[0]);
    assert!(parts[2].contains(&name));
}

#[test]
fn deleting_a_root_is_refused() {
    assert!(deletion_refused(Path::new("/")).is_some());
    #[cfg(windows)]
    assert!(deletion_refused(Path::new("C:\\")).is_some());
    assert!(deletion_refused(Path::new("/home/user/file.txt")).is_none());
}

#[test]
fn is_within_compares_path_components() {
    assert!(is_within(Path::new("/a/b"), Path::new("/a")));
    assert!(is_within(Path::new("/a"), Path::new("/a")));
    assert!(!is_within(Path::new("/ab"), Path::new("/a")));
    assert!(is_within(Path::new("/a/b/c"), Path::new("/")));
}

#[test]
fn listing_remaps_a_selection_by_name() {
    let before = listing(vec![
        entry("a", Kind::Dir, None),
        entry("b", Kind::File, Some(1)),
        entry("c", Kind::File, Some(2)),
    ]);
    let names = before.names_of(&[0, 2]);
    assert_eq!(names, vec![OsString::from("a"), OsString::from("c")]);

    // After a refresh that dropped "a", only "c" is left to select.
    let refreshed = listing(vec![
        entry("b", Kind::File, Some(1)),
        entry("c", Kind::File, Some(2)),
    ]);
    assert_eq!(refreshed.indices_of(&names), vec![1]);
}
