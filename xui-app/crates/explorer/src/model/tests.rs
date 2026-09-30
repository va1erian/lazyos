#![forbid(unsafe_code)]

//! Pure unit tests for the model: order, summaries, properties, formatting and
//! path helpers. None touches the disk or a widget.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use super::sort::sort_entries;
use super::*;
use crate::platform::{Kind, Meta};

fn entry(name: &str, kind: Kind, size: Option<u64>) -> Entry {
    let detail = match kind {
        Kind::Dir => "Folder".to_string(),
        _ => size.map(format_size).unwrap_or_else(|| "File".to_string()),
    };
    Entry {
        name: OsString::from(name),
        display: name.to_string(),
        detail,
        kind,
        size,
    }
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
    sort_entries(&mut entries);
    let names: Vec<&str> = entries.iter().map(|entry| entry.display.as_str()).collect();
    assert_eq!(names, ["Alpha", "apple", "beta.log", "zeta.txt"]);
}

#[test]
fn a_non_utf8_name_sorts_without_panicking() {
    let mut entries = vec![Entry {
        name: OsString::from("z"),
        display: "z".to_string(),
        detail: "File".to_string(),
        kind: Kind::File,
        size: None,
    }];
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        entries.push(Entry {
            name: OsString::from_vec(vec![0xff, 0x61]),
            display: OsString::from_vec(vec![0xff, 0x61])
                .to_string_lossy()
                .into_owned(),
            detail: "File".to_string(),
            kind: Kind::File,
            size: None,
        });
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let name = OsString::from_wide(&[0xD800, 0x0061]);
        entries.push(Entry {
            name: name.clone(),
            display: name.to_string_lossy().into_owned(),
            detail: "File".to_string(),
            kind: Kind::File,
            size: None,
        });
    }
    sort_entries(&mut entries);
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
    sort_entries(&mut entries);
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
fn the_model_reports_the_open_icon_only_while_the_folder_flashes() {
    use std::cell::Cell;
    use std::ffi::OsStr;
    use std::rc::Rc;
    use std::time::Instant;

    use xui_core::icon::{IconRef, Lucide};
    use xui_core::widget::IconModel;

    let now = Rc::new(Cell::new(Instant::now()));
    let clock: Clock = {
        let now = Rc::clone(&now);
        Rc::new(move || now.get())
    };
    let flash = Rc::new(Flash::with_clock(clock));
    let mut listing = Listing::empty(Path::new("/a"));
    listing.entries = vec![
        entry("docs", Kind::Dir, None),
        entry("notes.txt", Kind::File, Some(4)),
    ];
    let model = SharedListing::with_flash(Rc::new(listing), Rc::clone(&flash));

    assert_eq!(model.icon(0), Some(IconRef::Lucide(Lucide::Folder)));
    flash.flash(OsStr::new("docs"));
    assert_eq!(model.icon(0), Some(IconRef::Lucide(Lucide::FolderOpen)));
    assert_eq!(
        model.icon(1),
        Some(IconRef::Lucide(Lucide::File)),
        "a file never opens its icon"
    );

    now.set(now.get() + Duration::from_millis(2_000));
    assert_eq!(
        model.icon(0),
        Some(IconRef::Lucide(Lucide::Folder)),
        "the open icon reverts at the deadline"
    );
}

#[test]
fn listing_remaps_a_selection_by_name() {
    let mut listing = Listing::empty(Path::new("/a"));
    listing.entries = vec![
        entry("a", Kind::Dir, None),
        entry("b", Kind::File, Some(1)),
        entry("c", Kind::File, Some(2)),
    ];
    let names = listing.names_of(&[0, 2]);
    assert_eq!(names, vec![OsString::from("a"), OsString::from("c")]);

    // After a refresh that dropped "a", only "c" is left to select.
    let mut refreshed = Listing::empty(Path::new("/a"));
    refreshed.entries = vec![
        entry("b", Kind::File, Some(1)),
        entry("c", Kind::File, Some(2)),
    ];
    assert_eq!(refreshed.indices_of(&names), vec![1]);
}
