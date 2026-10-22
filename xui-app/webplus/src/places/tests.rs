//! The bookmark store: round trips through the file, damage, compaction, moves and caps.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::log::{self, Op, MAGIC};
use super::model::{
    Entry, Error, Kind, BAR, FIRST_ID, MAX_DEPTH, MAX_ENTRIES, MAX_TITLE, MAX_URL, OTHER, UNSORTED,
};
use super::store::COMPACT_MIN_RECORDS;
use super::Bookmarks;

/// A fresh folder in the system's temp directory, removed at the end of the test.
pub(super) struct Dir(pub PathBuf);

impl Dir {
    pub(super) fn new() -> Dir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lazywebplus-places-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        Dir(dir)
    }

    pub(super) fn file(&self) -> PathBuf {
        self.0.join("bookmarks.log")
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn link(parent: u64, title: &str, url: &str, position: u32, id: u64) -> Entry {
    Entry {
        id,
        parent,
        kind: Kind::Link,
        title: title.into(),
        url: url.into(),
        position,
        created: 1,
        modified: 1,
    }
}

fn log_of(ops: &[Op]) -> Vec<u8> {
    let mut bytes = MAGIC.to_vec();
    for op in ops {
        bytes.extend(log::encode(op));
    }
    bytes
}

#[test]
fn a_bookmark_survives_a_restart_with_all_its_fields() {
    let dir = Dir::new();
    let mut set = Bookmarks::open(dir.file());
    let id = set
        .add_link(UNSORTED, "Example", "https://example.com/a?b=1#c", 100)
        .unwrap();
    let folder = set.add_folder(BAR, "News", 101).unwrap();
    let other = set
        .add_link(folder, "日本語 é", "http://ex.test/", 102)
        .unwrap();
    set.add_separator(BAR, 103).unwrap();

    let back = Bookmarks::open(dir.file());
    assert_eq!(back.len(), 4);
    let e = back.get(id).unwrap();
    assert_eq!(
        (e.parent, e.kind, e.title.as_str(), e.url.as_str()),
        (
            UNSORTED,
            Kind::Link,
            "Example",
            "https://example.com/a?b=1#c"
        )
    );
    assert_eq!((e.created, e.modified), (100, 100));
    assert_eq!(back.get(other).unwrap().title, "日本語 é");
    assert_eq!(back.get(other).unwrap().parent, folder);
    assert_eq!(back.children(BAR).len(), 2);
    // Numbers are not reused after a restart.
    let mut back = back;
    let next = back.add_link(UNSORTED, "n", "http://n.test/", 5).unwrap();
    assert!(next > other);
}

#[test]
fn a_missing_empty_or_unwritable_file_is_an_empty_set() {
    let dir = Dir::new();
    assert!(Bookmarks::open(dir.file()).is_empty());
    fs::write(dir.file(), b"").unwrap();
    assert!(Bookmarks::open(dir.file()).is_empty());
    // A path that is a folder cannot be read as a file.
    let as_dir = dir.0.join("sub");
    fs::create_dir(&as_dir).unwrap();
    assert!(Bookmarks::open(as_dir).is_empty());
}

#[test]
fn a_cut_off_or_corrupt_tail_is_ignored_and_the_next_change_is_kept() {
    let dir = Dir::new();
    {
        let mut set = Bookmarks::open(dir.file());
        for n in 0..5 {
            set.add_link(UNSORTED, &format!("t{n}"), &format!("http://h{n}.test/"), n)
                .unwrap();
        }
    }
    // Cut inside the last record.
    let bytes = fs::read(dir.file()).unwrap();
    fs::write(dir.file(), &bytes[..bytes.len() - 5]).unwrap();
    let mut set = Bookmarks::open(dir.file());
    assert_eq!(set.len(), 4);
    // The damage was removed: a new record is read back after a restart.
    set.add_link(UNSORTED, "new", "http://new.test/", 9)
        .unwrap();
    assert_eq!(Bookmarks::open(dir.file()).len(), 5);

    // Garbage appended after whole records.
    let mut bytes = fs::read(dir.file()).unwrap();
    bytes.extend_from_slice(&[0xFF; 40]);
    fs::write(dir.file(), &bytes).unwrap();
    let mut set = Bookmarks::open(dir.file());
    assert_eq!(set.len(), 5);
    set.add_link(UNSORTED, "more", "http://more.test/", 9)
        .unwrap();
    assert_eq!(Bookmarks::open(dir.file()).len(), 6);
}

#[test]
fn a_flipped_byte_ends_the_log_there() {
    let dir = Dir::new();
    {
        let mut set = Bookmarks::open(dir.file());
        for n in 0..4 {
            set.add_link(UNSORTED, "t", &format!("http://h{n}.test/"), n)
                .unwrap();
        }
    }
    let mut bytes = fs::read(dir.file()).unwrap();
    let at = bytes.len() / 2;
    bytes[at] ^= 0x40;
    fs::write(dir.file(), &bytes).unwrap();
    let set = Bookmarks::open(dir.file());
    assert!(set.len() < 4);
}

#[test]
fn a_file_that_is_not_ours_is_set_aside_not_overwritten() {
    let dir = Dir::new();
    fs::write(dir.file(), b"<html>my precious notes</html>").unwrap();
    let mut set = Bookmarks::open(dir.file());
    assert!(set.is_empty());
    assert_eq!(
        fs::read(dir.0.join("bookmarks.log.bad")).unwrap(),
        b"<html>my precious notes</html>"
    );
    set.add_link(UNSORTED, "a", "http://a.test/", 1).unwrap();
    assert_eq!(Bookmarks::open(dir.file()).len(), 1);
}

#[test]
fn a_record_with_a_good_frame_and_bad_content_is_skipped_alone() {
    let dir = Dir::new();
    let mut bytes = MAGIC.to_vec();
    bytes.extend(log::encode(&Op::Put(link(
        UNSORTED,
        "ok",
        "http://ok.test/",
        0,
        4,
    ))));
    // A whole frame around a payload of an unknown kind.
    let junk = [1u8, 9, 9, 9];
    bytes.extend_from_slice(&(junk.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&log::crc32(&junk).to_le_bytes());
    bytes.extend_from_slice(&junk);
    bytes.extend(log::encode(&Op::Put(link(
        UNSORTED,
        "ok2",
        "http://ok2.test/",
        1,
        5,
    ))));
    fs::write(dir.file(), bytes).unwrap();
    let set = Bookmarks::open(dir.file());
    assert_eq!(set.len(), 2);
}

#[test]
fn what_a_hand_edited_file_holds_is_checked_like_what_is_typed() {
    let dir = Dir::new();
    let bytes = log_of(&[
        Op::Put(link(UNSORTED, "script", "javascript:alert(1)", 0, 4)),
        Op::Put(link(UNSORTED, "script2", "JaVa\tScRiPt:alert(1)", 1, 5)),
        Op::Put(link(UNSORTED, "t\u{202E}it\nle", "http://fine.test/", 2, 6)),
        // A root's number, and a loop of folders, and an orphan.
        Op::Put(link(UNSORTED, "root", "http://root.test/", 0, BAR)),
        Op::Put(Entry {
            kind: Kind::Folder,
            url: String::new(),
            ..link(8, "a", "", 0, 7)
        }),
        Op::Put(Entry {
            kind: Kind::Folder,
            url: String::new(),
            ..link(7, "b", "", 0, 8)
        }),
        Op::Put(link(999, "orphan", "http://orphan.test/", 0, 9)),
        Op::Put(link(6, "under a link", "http://u.test/", 0, 10)),
    ]);
    fs::write(dir.file(), bytes).unwrap();
    let set = Bookmarks::open(dir.file());
    assert!(set.find_url("javascript:alert(1)").is_none());
    assert_eq!(set.get(4), None);
    assert_eq!(set.get(5), None);
    assert_eq!(set.get(6).unwrap().title, "title");
    assert_eq!(set.get(BAR), None);
    // The folder loop was broken, not dropped; the orphans went to Unsorted.
    assert!(set.get(7).is_some() && set.get(8).is_some());
    for id in [7, 8, 9, 10] {
        let mut at = id;
        let mut steps = 0;
        while at > 3 {
            at = set.get(at).unwrap().parent;
            steps += 1;
            assert!(steps <= MAX_DEPTH, "entry {id} never reaches a root");
        }
    }
    assert_eq!(set.get(9).unwrap().parent, UNSORTED);
    assert_eq!(set.get(10).unwrap().parent, UNSORTED);
}

#[test]
fn compaction_rewrites_a_mostly_dead_file_and_keeps_every_live_entry() {
    let dir = Dir::new();
    let mut set = Bookmarks::open(dir.file());
    let keep = set
        .add_link(UNSORTED, "keep", "http://keep.test/", 1)
        .unwrap();
    let folder = set.add_folder(BAR, "f", 1).unwrap();
    let inside = set.add_link(folder, "in", "http://in.test/", 1).unwrap();
    // Edits and removals make dead records.
    let temp = set.add_link(UNSORTED, "x", "http://x.test/", 1).unwrap();
    let mut peak = 0;
    for n in 0..(COMPACT_MIN_RECORDS * 2) {
        set.edit(temp, &format!("x{n}"), "http://x.test/", n as u64)
            .unwrap();
        peak = peak.max(set.records());
        // Never more than half dead for long (below the minimum a file is left alone).
        assert!(set.records() < COMPACT_MIN_RECORDS || set.records() <= 2 * set.len());
    }
    set.remove(temp).unwrap();
    for n in 0..COMPACT_MIN_RECORDS {
        let id = set
            .add_link(UNSORTED, "t", &format!("http://t{n}.test/"), 2)
            .unwrap();
        set.remove(id).unwrap();
        peak = peak.max(set.records());
    }
    assert!(set.records() < peak, "the file was rewritten at least once");
    assert!(set.records() < COMPACT_MIN_RECORDS || set.records() <= 2 * set.len());
    assert_eq!(set.records(), set.len() + dead_in(&set, &dir));
    assert!(!dir.0.join("bookmarks.log.tmp").exists());

    let back = Bookmarks::open(dir.file());
    assert_eq!(back.len(), 3);
    assert_eq!(back.get(keep).unwrap().url, "http://keep.test/");
    assert_eq!(back.get(inside).unwrap().parent, folder);
    assert_eq!(back.get(folder).unwrap().parent, BAR);
}

/// Records in the file beyond one per live entry.
fn dead_in(set: &Bookmarks, dir: &Dir) -> usize {
    let parsed = log::parse(&fs::read(dir.file()).unwrap());
    parsed.records - set.len()
}

#[test]
fn a_small_file_is_not_rewritten_and_an_explicit_compaction_is_exact() {
    let dir = Dir::new();
    let mut set = Bookmarks::open(dir.file());
    let id = set.add_link(UNSORTED, "a", "http://a.test/", 1).unwrap();
    set.edit(id, "b", "http://a.test/", 2).unwrap();
    assert_eq!(set.records(), 2);
    set.compact().unwrap();
    assert_eq!(set.records(), 1);
    assert_eq!(log::parse(&fs::read(dir.file()).unwrap()).records, 1);
}

#[test]
fn moves_between_folders_keep_the_order_of_what_is_left() {
    let mut set = Bookmarks::in_memory();
    let a = set.add_link(BAR, "a", "http://a.test/", 1).unwrap();
    let b = set.add_link(BAR, "b", "http://b.test/", 1).unwrap();
    let c = set.add_link(BAR, "c", "http://c.test/", 1).unwrap();
    let folder = set.add_folder(OTHER, "f", 1).unwrap();
    let ids = |set: &Bookmarks, p| set.children(p).iter().map(|e| e.id).collect::<Vec<_>>();
    assert_eq!(ids(&set, BAR), [a, b, c]);

    set.move_to(c, BAR, 0, 2).unwrap();
    assert_eq!(ids(&set, BAR), [c, a, b]);
    set.move_to(a, folder, 99, 3).unwrap();
    assert_eq!(ids(&set, BAR), [c, b]);
    assert_eq!(ids(&set, folder), [a]);
    set.move_to(b, folder, 0, 4).unwrap();
    assert_eq!(ids(&set, folder), [b, a]);
    assert_eq!(set.get(b).unwrap().modified, 4);
    // Into Unsorted, a root.
    set.move_to(folder, UNSORTED, 0, 5).unwrap();
    assert_eq!(set.get(folder).unwrap().parent, UNSORTED);
}

#[test]
fn a_move_that_would_break_the_tree_is_refused() {
    let mut set = Bookmarks::in_memory();
    let outer = set.add_folder(BAR, "outer", 1).unwrap();
    let inner = set.add_folder(outer, "inner", 1).unwrap();
    let leaf = set.add_link(inner, "l", "http://l.test/", 1).unwrap();
    assert!(matches!(
        set.move_to(outer, inner, 0, 2),
        Err(Error::BadParent)
    ));
    assert!(matches!(
        set.move_to(outer, outer, 0, 2),
        Err(Error::BadParent)
    ));
    assert!(matches!(
        set.move_to(inner, leaf, 0, 2),
        Err(Error::BadParent)
    ));
    assert!(matches!(
        set.move_to(leaf, 12345, 0, 2),
        Err(Error::BadParent)
    ));
    assert!(matches!(
        set.move_to(BAR, OTHER, 0, 2),
        Err(Error::NoSuchEntry)
    ));
    assert!(matches!(
        set.move_to(777, BAR, 0, 2),
        Err(Error::NoSuchEntry)
    ));
    assert_eq!(set.get(outer).unwrap().parent, BAR);
}

#[test]
fn folders_nest_only_so_deep() {
    let mut set = Bookmarks::in_memory();
    let mut parent = BAR;
    let mut made = 0;
    while let Ok(id) = set.add_folder(parent, "d", 1) {
        parent = id;
        made += 1;
        assert!(made <= MAX_DEPTH);
    }
    assert!(made >= MAX_DEPTH - 1);
    // A link still fits in the deepest folder; the whole stack cannot move under another folder.
    assert!(set.add_link(parent, "l", "http://l.test/", 1).is_ok());
    let side = set.add_folder(OTHER, "side", 1).unwrap();
    let first = set.children(BAR)[0].id;
    assert!(matches!(
        set.move_to(first, side, 0, 2),
        Err(Error::BadParent)
    ));
}

#[test]
fn removing_a_folder_removes_what_is_in_it() {
    let dir = Dir::new();
    let mut set = Bookmarks::open(dir.file());
    let folder = set.add_folder(BAR, "f", 1).unwrap();
    let sub = set.add_folder(folder, "s", 1).unwrap();
    set.add_link(sub, "deep", "http://deep.test/", 1).unwrap();
    set.add_link(folder, "mid", "http://mid.test/", 1).unwrap();
    let keep = set.add_link(BAR, "keep", "http://keep.test/", 1).unwrap();
    assert_eq!(set.remove(folder).unwrap(), 4);
    assert_eq!(set.len(), 1);
    assert!(matches!(set.remove(folder), Err(Error::NoSuchEntry)));
    assert!(matches!(set.remove(BAR), Err(Error::NoSuchEntry)));
    let back = Bookmarks::open(dir.file());
    assert_eq!(back.len(), 1);
    assert!(back.get(keep).is_some());
}

#[test]
fn editing_changes_a_link_and_a_folder_but_not_a_root_or_a_separator() {
    let mut set = Bookmarks::in_memory();
    let l = set.add_link(UNSORTED, "a", "http://a.test/", 1).unwrap();
    let f = set.add_folder(UNSORTED, "f", 1).unwrap();
    let s = set.add_separator(UNSORTED, 1).unwrap();
    set.edit(l, "  new\ttitle ", "http://b.test/", 7).unwrap();
    assert_eq!(set.get(l).unwrap().title, "newtitle");
    assert_eq!(set.get(l).unwrap().url, "http://b.test/");
    assert_eq!(set.get(l).unwrap().modified, 7);
    assert_eq!(set.get(l).unwrap().created, 1);
    set.edit(f, "renamed", "ignored", 8).unwrap();
    assert_eq!(set.get(f).unwrap().url, "");
    assert!(set.edit(s, "x", "", 9).is_err());
    assert!(set.edit(BAR, "x", "", 9).is_err());
    assert!(matches!(
        set.edit(l, "t", "javascript:1", 9),
        Err(Error::Unsupported)
    ));
    assert_eq!(set.get(l).unwrap().url, "http://b.test/");
}

#[test]
fn sizes_and_addresses_are_capped() {
    let mut set = Bookmarks::in_memory();
    let long: String = "é".repeat(MAX_TITLE + 100);
    let id = set.add_link(UNSORTED, &long, "http://a.test/", 1).unwrap();
    assert_eq!(set.get(id).unwrap().title.chars().count(), MAX_TITLE);

    let ok = format!("http://a.test/{}", "x".repeat(MAX_URL - 14));
    assert_eq!(ok.len(), MAX_URL);
    assert!(set.add_link(UNSORTED, "t", &ok, 1).is_ok());
    assert!(matches!(
        set.add_link(UNSORTED, "t", &format!("{ok}x"), 1),
        Err(Error::UrlTooLong)
    ));
    for bad in [
        "",
        "   ",
        "javascript:alert(1)",
        "  JAVASCRIPT:alert(1)",
        "java\nscript:alert(1)",
        "java\tscript:alert(1)",
        "\u{202E}javascript:x",
    ] {
        assert!(
            matches!(set.add_link(UNSORTED, "t", bad, 1), Err(Error::Unsupported)),
            "{bad:?}"
        );
    }
    // Only web pages are ever opened from the bookmarks.
    for other in ["mailto:a@b.test", "file:///etc/passwd", "ftp://x.test/"] {
        assert!(matches!(
            set.add_link(UNSORTED, "t", other, 1),
            Err(Error::Unsupported)
        ));
    }
    assert!(set
        .add_link(UNSORTED, "t", "https://javascript.test/", 1)
        .is_ok());
}

#[test]
fn the_number_of_entries_is_capped() {
    // Past the cap by loading, which is cheaper than adding 100,000 one by one.
    let ops: Vec<Op> = (0..MAX_ENTRIES as u64 + 50)
        .map(|n| Op::Put(link(UNSORTED, "t", "http://t.test/", 0, FIRST_ID + n)))
        .collect();
    let dir = Dir::new();
    let mut bytes = MAGIC.to_vec();
    for op in &ops {
        bytes.extend(log::encode(op));
    }
    fs::write(dir.file(), bytes).unwrap();
    let mut set = Bookmarks::open(dir.file());
    assert_eq!(set.len(), MAX_ENTRIES);
    assert!(matches!(
        set.add_link(UNSORTED, "t", "http://x.test/", 1),
        Err(Error::Full)
    ));
}

#[test]
fn a_change_that_cannot_be_written_is_undone_and_says_so() {
    let dir = Dir::new();
    // The "file" is a path inside a regular file: nothing can be created there.
    let blocker = dir.0.join("blocker");
    fs::write(&blocker, b"x").unwrap();
    let mut set = Bookmarks::open(blocker.join("bookmarks.log"));
    let result = set.add_link(UNSORTED, "a", "http://a.test/", 1);
    assert!(matches!(result, Err(Error::Io(_))));
    assert_eq!(set.len(), 0, "memory matches the file");
    assert_eq!(set.version(), 0);
}

#[test]
fn find_url_matches_exactly_and_prefers_the_oldest() {
    let mut set = Bookmarks::in_memory();
    let first = set.add_link(UNSORTED, "a", "http://a.test/", 1).unwrap();
    set.add_link(BAR, "a again", "http://a.test/", 2).unwrap();
    assert_eq!(set.find_url("http://a.test/").unwrap().id, first);
    assert!(set.find_url("http://a.test").is_none());
}
