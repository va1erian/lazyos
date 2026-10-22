//! A second name for a file made a moment ago (issue #864): WebKit's blob
//! storage creates a file, fills it and links it, once per cached body, and a
//! barrier per link (a writeback of the file's data and a device flush, under
//! the volume lock) turned each into a stall. A file whose inode no writeback
//! has written since it was allocated has no link count on the disk to be too
//! low, so `link` skips the barrier for it (`cache/fresh.rs`); every other file
//! keeps it.
//!
//! The cases: the barrier is skipped exactly for a fresh inode, an inode a
//! writeback or an `fsync` has written is not fresh any more, a piecewise
//! commit's snapshot makes every inode allocated before it not fresh, and
//! across every cut of the disk a file that was on the disk before the window
//! never has fewer links than names while fresh links are made around it.

use std::format;
use std::string::String;
use std::sync::Arc;
use std::vec::Vec;

use super::cache_crash::{image_at, judge, Recorder};
use super::*;
use crate::{CacheConfig, Owner, Progress};

const BLOCK: usize = 1024;

fn body(tag: u8) -> Vec<u8> {
    std::vec![tag; 3000]
}

struct Volume {
    disk: MemIo,
    recorder: Recorder,
    fs: Ext2,
}

impl Volume {
    fn new(cache_blocks: usize) -> Volume {
        let disk = formatted(2 << 20, BLOCK as u32);
        let recorder = Recorder {
            disk: disk.clone(),
            block_size: BLOCK,
            log: Arc::default(),
        };
        let fs = Ext2::open_cached(
            Box::new(recorder.clone()),
            clock,
            CacheConfig::heap(cache_blocks),
        )
        .unwrap();
        Volume { disk, recorder, fs }
    }

    fn put(&self, path: &str, tag: u8) {
        self.fs
            .write_file(path, &body(tag), 0o644, 0, 0, 1)
            .unwrap();
    }

    /// Block writes reached the disk so far.
    fn writes(&self) -> usize {
        self.recorder.log.lock().unwrap().len()
    }

    /// Block writes `link(from, to)` caused.
    fn link_writes(&self, from: &str, to: &str) -> usize {
        let before = self.writes();
        self.fs.link(from, to).unwrap();
        self.writes() - before
    }
}

#[test]
fn a_file_made_just_now_is_linked_without_writing_anything() {
    let v = Volume::new(64);
    v.put("/f", 7);
    assert_eq!(v.link_writes("/f", "/g"), 0);
    // A third name, and one in another directory: still nothing is written.
    v.fs.mkdir("/d", 0o755, Owner::ROOT).unwrap();
    assert_eq!(v.link_writes("/f", "/d/h"), 0);
    assert_eq!(v.fs.link_count("/f").unwrap(), 3);
    for name in ["/f", "/g", "/d/h"] {
        assert!(holds(&v.fs, name), "{name}");
    }
    v.fs.flush().unwrap();
    assert!(fsck(&v.disk.snapshot()).is_empty());
    assert_eq!(v.fs.link_count("/g").unwrap(), 3);
}

#[test]
fn a_file_a_writeback_has_written_keeps_the_barrier() {
    let v = Volume::new(64);
    v.put("/f", 7);
    v.fs.flush().unwrap();
    assert!(v.link_writes("/f", "/g") > 0, "a durable file is ordered");

    // `fsync` writes the inode alone: from then on it is on the disk.
    v.put("/h", 8);
    let handle = v.fs.open_file("/h").unwrap();
    v.fs.sync_file(handle).unwrap();
    assert!(v.link_writes("/h", "/i") > 0, "an fsynced file is ordered");

    // A plain writeback of everything: the same.
    v.put("/j", 9);
    v.fs.writeback().unwrap();
    assert!(
        v.link_writes("/j", "/k") > 0,
        "a written-back file is ordered"
    );

    // The next file made is fresh again.
    v.put("/l", 10);
    assert_eq!(v.link_writes("/l", "/m"), 0);
}

#[test]
fn another_files_fsync_leaves_this_one_fresh() {
    let v = Volume::new(64);
    v.put("/a", 1);
    v.put("/b", 2);
    let handle = v.fs.open_file("/a").unwrap();
    v.fs.sync_file(handle).unwrap();
    // `/a`'s inode went alone; `/b`'s slot is still the disk's own.
    assert!(v.link_writes("/a", "/a2") > 0);
    assert_eq!(v.link_writes("/b", "/b2"), 0);
}

#[test]
fn a_piecewise_commit_forgets_the_inodes_it_may_write() {
    let v = Volume::new(256);
    v.put("/before", 3);
    // The snapshot is taken here: `/before`'s inode may land in any piece.
    assert!(matches!(
        v.fs.writeback_piece(&mut || true).unwrap(),
        Progress::More
    ));
    assert!(v.link_writes("/before", "/before2") > 0);
    // One allocated after it is in no piece: it is fresh.
    v.put("/after", 4);
    assert_eq!(v.link_writes("/after", "/after2"), 0);
    while matches!(v.fs.writeback_piece(&mut || true).unwrap(), Progress::More) {}
    v.fs.flush().unwrap();
    assert!(fsck(&v.disk.snapshot()).is_empty());
}

#[test]
fn an_uncached_volume_has_no_fresh_inodes() {
    let disk = formatted(2 << 20, BLOCK as u32);
    let fs = open(&disk);
    fs.write_file("/f", &body(1), 0o644, 0, 0, 1).unwrap();
    let ino = fs.resolve("/f").unwrap();
    assert!(!fs.inode_is_fresh(ino));
    fs.link("/f", "/g").unwrap();
    assert!(fsck(&disk.snapshot()).is_empty());
}

fn holds(fs: &Ext2, path: &str) -> bool {
    let mut buf = std::vec![0u8; 3001];
    fs.read(path, 0, &mut buf).is_ok_and(|n| n == 3000)
        && buf[..3000].iter().all(|&byte| byte == buf[0])
}

/// The `(links, entries)` the checker reports for every inode of a crashed
/// image whose link count differs from its names.
fn count_problems(image: &[u8]) -> Vec<(u32, u32, u32)> {
    fsck(image)
        .iter()
        .filter_map(|problem| {
            let rest = problem.strip_prefix("inode ")?;
            let (ino, counts) = rest.split_once(": links ")?;
            let (links, entries) = counts.split_once(", entries ")?;
            Some((
                ino.parse().ok()?,
                links.parse().ok()?,
                entries.parse().ok()?,
            ))
        })
        .collect()
}

/// Judge every cut of the writes in `log` the way `cache_crash` does, and
/// hold every inode that is on the disk with a link count to the promise of a
/// link: never fewer links than names. (An inode whose slot the disk never
/// got, count 0, is the documented "initialisation was lost" class that a
/// file's first name is in as well.)
fn sweep(base: &[u8], log: &[(u64, Vec<u8>)]) {
    for point in 0..=log.len() {
        let image = image_at(base, log, point);
        for (ino, links, entries) in count_problems(&image) {
            assert!(
                links == 0 || links >= entries,
                "cut after write {point}: inode {ino} has {links} links for {entries} names"
            );
        }
        judge(image, point);
    }
}

/// Fresh files linked beside a durable file that is linked too, in a
/// directory that keeps growing: every cut keeps the durable file's count.
fn window(v: &Volume, between: &mut dyn FnMut(&Volume, usize)) -> Vec<u8> {
    v.fs.mkdir("/a", 0o755, Owner::ROOT).unwrap();
    v.fs.mkdir("/b", 0o755, Owner::ROOT).unwrap();
    v.put("/a/d", 200);
    v.fs.flush().unwrap();
    let base = v.disk.snapshot();
    v.recorder.log.lock().unwrap().clear();
    for n in 0..30 {
        let path = format!("/a/n{n}");
        v.put(&path, 1 + n as u8);
        between(v, n);
        // The names grow `/b` by a block every so often (1 KiB blocks).
        v.fs.link(
            &path,
            &format!("/b/a-long-enough-name-for-a-block-to-fill-{n}"),
        )
        .unwrap();
        if n % 7 == 3 {
            v.fs.link("/a/d", &format!("/b/d{n}")).unwrap();
        }
    }
    v.fs.flush().unwrap();
    base
}

#[test]
fn every_cut_keeps_a_durable_files_count_beside_fresh_links() {
    for cache in [4, 16, 64] {
        let v = Volume::new(cache);
        let base = window(&v, &mut |v, n| {
            // A plain commit lands now and then, between a create and its link.
            if n % 11 == 5 {
                v.fs.writeback().unwrap();
            }
        });
        let log = v.recorder.log.lock().unwrap().clone();
        sweep(&base, &log);
        let end = fsck(&v.disk.snapshot());
        assert!(end.is_empty(), "{cache}: {end:?}");
        assert_eq!(v.fs.link_count("/a/d").unwrap(), 1 + 4); // n = 3, 10, 17, 24
    }
}

#[test]
fn every_cut_between_commit_pieces_keeps_a_durable_files_count() {
    for cache in [16, 64, 256] {
        let v = Volume::new(cache);
        let base = window(&v, &mut |v, n| {
            // A piece of a commit lands between the create and the link, so
            // the snapshot falls on some files and not on others.
            if n % 3 == 1 {
                let _ = v.fs.writeback_piece(&mut || true).unwrap();
            }
        });
        let log = v.recorder.log.lock().unwrap().clone();
        sweep(&base, &log);
        let end = fsck(&v.disk.snapshot());
        assert!(end.is_empty(), "{cache}: {end:?}");
    }
}

/// A commit in pieces has written inode `x` (count 1) when a second name for
/// it is made, and the name needs a new directory block, which the next
/// writeback writes ahead of every inode. Were `x` still taken for fresh, the
/// name would be on the disk against a count of 1 in between. For each place
/// in the commit the link can fall.
#[test]
fn a_name_never_outruns_the_count_of_an_inode_a_snapshot_wrote() {
    // Names so long that three of them fill a directory block.
    let long = |tag: &str| format!("/b/{}{tag}", "n".repeat(240));
    for after_requests in 0..6 {
        let v = Volume::new(256);
        v.fs.mkdir("/a", 0o755, Owner::ROOT).unwrap();
        v.fs.mkdir("/b", 0o755, Owner::ROOT).unwrap();
        for (n, tag) in ["1", "2", "3"].into_iter().enumerate() {
            v.put(&long(tag), 10 + n as u8);
        }
        v.fs.flush().unwrap();
        let base = v.disk.snapshot();
        v.recorder.log.lock().unwrap().clear();

        v.put("/a/x", 99);
        let mut requests = 0;
        let mut linked = false;
        loop {
            if requests == after_requests && !linked {
                v.fs.link("/a/x", &long("4")).unwrap();
                linked = true;
            }
            match v.fs.writeback_piece(&mut || true).unwrap() {
                Progress::Done => break,
                Progress::More => requests += 1,
            }
        }
        if !linked {
            v.fs.link("/a/x", &long("4")).unwrap();
        }
        v.fs.flush().unwrap();
        let log = v.recorder.log.lock().unwrap().clone();
        sweep(&base, &log);
        assert!(fsck(&v.disk.snapshot()).is_empty());
    }
}

/// An inode number given back by a failed `create` (the volume was full when
/// the directory needed a block) is reused by the next file, which is fresh:
/// the slot the failed create left on the disk must not carry a link count
/// that a second name outruns (`Ext2::abandon_inode`).
#[test]
fn a_recycled_inode_of_a_failed_create_holds_no_count_for_a_second_name() {
    let long = |tag: &str| format!("/d/{}{tag}", "n".repeat(240));
    let v = Volume::new(64);
    v.fs.mkdir("/d", 0o755, Owner::ROOT).unwrap();
    v.fs.mkdir("/e", 0o755, Owner::ROOT).unwrap();
    // Three such names fill `/d`'s block, so a fourth needs a new one.
    for (n, tag) in ["1", "2", "3"].into_iter().enumerate() {
        v.put(&long(tag), 10 + n as u8);
    }
    // Use up every free block (a file of a few blocks at a time, so the
    // volume runs out of blocks before it runs out of inodes).
    let mut filled = 0;
    while v
        .fs
        .write_file(&format!("/fill{filled}"), &[1; 64 * 1024], 0o644, 0, 0, 1)
        .is_ok()
    {
        filled += 1;
    }
    while v
        .fs
        .write_file(&format!("/fill{filled}"), &[1; 1024], 0o644, 0, 0, 1)
        .is_ok()
    {
        filled += 1;
    }
    assert_eq!(v.fs.free_blocks(), Ok(0), "no block left");
    let failed = v.fs.create(&long("4"), 0o644, Owner::ROOT);
    assert_eq!(failed.err(), Some(crate::Ext2Error::NoSpace));
    v.fs.flush().unwrap();
    // Space comes back without an inode number doing so.
    for n in 0..filled {
        v.fs.truncate(&format!("/fill{n}"), 0).unwrap();
    }
    v.fs.flush().unwrap();
    let base = v.disk.snapshot();
    v.recorder.log.lock().unwrap().clear();

    v.put("/e/g1", 77);
    assert_eq!(
        v.link_writes("/e/g1", "/e/g2"),
        0,
        "the recycled inode is fresh"
    );
    // Another file's `fsync` writes the directory blocks nobody owns.
    v.put("/other", 78);
    let handle = v.fs.open_file("/other").unwrap();
    v.fs.sync_file(handle).unwrap();
    v.fs.flush().unwrap();
    let log = v.recorder.log.lock().unwrap().clone();
    sweep(&base, &log);
    assert!(fsck(&v.disk.snapshot()).is_empty());
}

/// The names of the files a window made all read back, the fresh ones included.
#[test]
fn names_of_fresh_files_survive_the_end_of_a_window() {
    let v = Volume::new(64);
    window(&v, &mut |_, _| {});
    let mut names: Vec<String> = Vec::new();
    for n in 0..30 {
        names.push(format!("/a/n{n}"));
        names.push(format!("/b/a-long-enough-name-for-a-block-to-fill-{n}"));
    }
    for name in &names {
        assert!(holds(&v.fs, name), "{name}");
        assert_eq!(v.fs.link_count(name).unwrap(), 2, "{name}");
    }
}
