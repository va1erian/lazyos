//! A write that fails partway: the file is cut back, the change undone, and what cannot be cut
//! back stops further appends. Plus what a loaded file may hold.

use std::fs;

use super::log::{self, Op, MAGIC};
use super::model::{Entry, Error, Kind, UNSORTED};
use super::tests::Dir;
use super::Bookmarks;

fn entry(id: u64, url: &str) -> Entry {
    Entry {
        id,
        parent: UNSORTED,
        kind: Kind::Link,
        title: "t".into(),
        url: url.into(),
        position: 0,
        created: 1,
        modified: 1,
    }
}

#[test]
fn a_torn_append_is_cut_back_and_the_next_change_survives_a_reopen() {
    let dir = Dir::new();
    let mut set = Bookmarks::open(dir.file());
    set.add_link(UNSORTED, "a", "http://a.test/", 1).unwrap();
    let whole = fs::metadata(dir.file()).unwrap().len();

    set.fault.tear_after = Some(5);
    let result = set.add_link(UNSORTED, "b", "http://b.test/", 2);
    assert!(matches!(result, Err(Error::Io(_))));
    assert_eq!(set.len(), 1, "the failed change is undone");
    assert_eq!(set.records(), 1);
    assert_eq!(fs::metadata(dir.file()).unwrap().len(), whole, "cut back");

    set.fault.tear_after = None;
    set.add_link(UNSORTED, "c", "http://c.test/", 3).unwrap();
    let again = Bookmarks::open(dir.file());
    let urls: Vec<_> = again.iter().map(|e| e.url.as_str()).collect();
    assert_eq!(urls, ["http://a.test/", "http://c.test/"]);
}

#[test]
fn torn_bytes_that_cannot_be_cut_stop_appends_until_a_rewrite_works() {
    let dir = Dir::new();
    let mut set = Bookmarks::open(dir.file());
    set.add_link(UNSORTED, "a", "http://a.test/", 1).unwrap();

    set.fault.tear_after = Some(5);
    set.fault.truncate_fails = true;
    assert!(set.add_link(UNSORTED, "b", "http://b.test/", 2).is_err());
    assert_eq!(set.len(), 1);

    // The file now ends in garbage: the next change is written by a rewrite, not an append.
    set.fault = Default::default();
    set.add_link(UNSORTED, "c", "http://c.test/", 3).unwrap();
    let again = Bookmarks::open(dir.file());
    let urls: Vec<_> = again.iter().map(|e| e.url.as_str()).collect();
    assert_eq!(urls, ["http://a.test/", "http://c.test/"]);
}

#[test]
fn only_web_addresses_are_loaded_from_the_file() {
    let dir = Dir::new();
    let mut bytes = MAGIC.to_vec();
    for (id, url) in [
        (10, "http://a.test/"),
        (11, "file:///etc/passwd"),
        (12, "mailto:x@y.test"),
        (13, "data:text/html,hi"),
        (14, "https://b.test/"),
    ] {
        bytes.extend(log::encode(&Op::Put(entry(id, url))));
    }
    fs::write(dir.file(), bytes).unwrap();
    let set = Bookmarks::open(dir.file());
    let ids: Vec<_> = set.iter().map(|e| e.id).collect();
    assert_eq!(ids, [10, 14]);
}
