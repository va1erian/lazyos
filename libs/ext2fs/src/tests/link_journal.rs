//! Hard links on a journaled volume (issue #871): a power cut at any write of
//! `link`, of `unlink` or `rename` of one name of a linked file, or of a
//! sequence of them, must replay to a volume the independent checker finds
//! *nothing* wrong with (the journal's bar, stricter than the unjournaled
//! "never a count below its names" of `link_crash.rs`), and every surviving
//! name of a linked file must still read its data.
//!
//! That bar holds for a cache that can hold an operation's metadata. A cache
//! of a few pages may have to commit in the middle of an operation, when
//! every page holds metadata (`docs/architecture/journal.md`, "Making room
//! mid-operation"): that transaction ends inside the operation, as every
//! write of an unjournaled volume does. Since each operation writes its
//! blocks in its crash-safe order (a count rises before its new name, a name
//! goes before its count drops), such a commit holds a prefix of that order:
//! a count too high at worst, never one below its names, and `Ext2::repair`
//! settles it. [`judge`] holds a tiny cache to exactly that.

use std::sync::Arc;

use fuzzkit::{for_seeds, Rng};

use super::cache_crash::{holds, image_at, Recorder};
use super::journal::{journaled, mount_and_settle};
use super::link_crash::{classify, Verdict};
use super::workload;
use super::*;
use crate::{CacheConfig, Owner};

/// Pages a cache needs so that no operation here commits mid-way.
const ROOMY: usize = 64;

/// Judge a replayed image of a volume cached in `blocks` pages: clean with a
/// roomy cache; with a tiny one never a count below its names, and what is
/// left (counts too high, unreachable inodes) repaired to a clean volume.
fn judge(image: Vec<u8>, blocks: usize, what: &str) -> Verdict {
    let verdict = classify(&image);
    match &verdict {
        Verdict::Clean => {}
        Verdict::TooLow(why) => panic!("{what}: {why}"),
        Verdict::Leak if blocks >= ROOMY => {
            panic!("{what}: a cache of {blocks} pages left {:#?}", fsck(&image))
        }
        Verdict::Leak => {
            let io = MemIo::from_bytes(image);
            let fs = open(&io);
            fs.repair()
                .unwrap_or_else(|error| panic!("{what}: repair refused: {error:?}"));
            fs.flush().unwrap();
            drop(fs);
            let left = fsck(&io.snapshot());
            assert!(left.is_empty(), "{what}: after the repair: {left:#?}");
        }
    }
    verdict
}

const DATA: [u8; 3000] = [7; 3000];

fn put(fs: &Ext2, path: &str, byte: u8) {
    fs.write_file(path, &[byte; 3000], 0o644, 0, 0, 1).unwrap();
}

fn mkdirs(fs: &Ext2, dirs: &[&str]) {
    for dir in dirs {
        fs.mkdir(dir, 0o755, Owner::ROOT).unwrap();
    }
}

/// Build with `setup`, record `op` (and its flush) through a cache of `blocks`
/// blocks, then cut at every write: each image replays to what [`judge`]
/// accepts, and at least one of the names in `shared` survives, every
/// survivor reading [`DATA`].
fn sweep(setup: fn(&Ext2), op: fn(&Ext2), shared: &[&str], blocks: usize) {
    let disk = journaled(2 << 20, 1024);
    let recorder = Recorder {
        disk: disk.clone(),
        block_size: 1024,
        log: Arc::default(),
    };
    let fs =
        Ext2::open_cached(Box::new(recorder.clone()), clock, CacheConfig::heap(blocks)).unwrap();
    setup(&fs);
    fs.flush().unwrap();
    let base = disk.snapshot();
    let start = recorder.log.lock().unwrap().len();
    op(&fs);
    fs.flush().unwrap();
    drop(fs);
    let log = recorder.log.lock().unwrap()[start..].to_vec();
    assert!(!log.is_empty(), "the operation wrote nothing");
    for point in 0..=log.len() {
        let (image, _) = mount_and_settle(image_at(&base, &log, point));
        judge(
            image.clone(),
            blocks,
            &std::format!("cut after write {point}"),
        );
        let fs = open(&MemIo::from_bytes(image));
        let live: Vec<_> = shared.iter().filter(|n| fs.lookup(n).is_ok()).collect();
        assert!(
            !live.is_empty(),
            "cut {point}: the linked file lost every name"
        );
        for name in &live {
            assert!(holds(&fs, name, &DATA), "cut {point}: {name} lost its data");
        }
    }
}

fn every_cache(setup: fn(&Ext2), op: fn(&Ext2), shared: &[&str]) {
    for blocks in [4, ROOMY] {
        sweep(setup, op, shared, blocks);
    }
}

#[test]
fn a_link_replays_whole_or_not_at_all() {
    every_cache(
        |fs| {
            mkdirs(fs, &["/a", "/b"]);
            put(fs, "/a/f", 7);
        },
        |fs| fs.link("/a/f", "/b/g").unwrap(),
        &["/a/f", "/b/g"],
    );
}

#[test]
fn unlinking_one_of_two_names_replays_clean() {
    every_cache(
        |fs| {
            mkdirs(fs, &["/a", "/b"]);
            put(fs, "/a/f", 7);
            fs.link("/a/f", "/b/g").unwrap();
        },
        |fs| fs.unlink("/a/f").unwrap(),
        &["/a/f", "/b/g"],
    );
}

#[test]
fn renames_over_and_of_linked_names_replay_clean() {
    every_cache(
        |fs| {
            mkdirs(fs, &["/a", "/b"]);
            put(fs, "/a/f", 7);
            fs.link("/a/f", "/b/g").unwrap();
            put(fs, "/a/t", 9);
        },
        |fs| {
            // Onto a second name of the same file: a no-op, both stay.
            fs.rename("/a/f", "/b/g").unwrap();
            // A linked file moves; another file replaces one of its names.
            fs.rename("/b/g", "/b/h").unwrap();
            fs.rename("/a/t", "/a/f").unwrap();
        },
        &["/b/g", "/b/h"],
    );
}

/// One random link/unlink/rename sequence, with writebacks between some
/// operations, through a cache of 4 or [`ROOMY`] pages; then `cuts` random
/// cut points (every one when `None`) are judged. Returns how many cuts were
/// clean and how many only leaked.
fn random_sequence(rng: &mut Rng, cuts: Option<usize>) -> (usize, usize) {
    let disk = journaled(2 << 20, 1024);
    let recorder = Recorder {
        disk: disk.clone(),
        block_size: 1024,
        log: Arc::default(),
    };
    let blocks = [4, ROOMY][rng.below(2) as usize];
    let fs =
        Ext2::open_cached(Box::new(recorder.clone()), clock, CacheConfig::heap(blocks)).unwrap();
    const DIRS: [&str; 3] = ["/a", "/b", "/a/x"];
    mkdirs(&fs, &DIRS);
    for n in 0..4u8 {
        put(
            &fs,
            &std::format!("{}/s{n}", DIRS[rng.below(3) as usize]),
            n + 1,
        );
    }
    fs.flush().unwrap();
    let base = disk.snapshot();
    let start = recorder.log.lock().unwrap().len();
    for _ in 0..rng.range(6, 24) {
        let mut name = || std::format!("{}/n{}", DIRS[rng.below(3) as usize], rng.below(4));
        let (from, to) = (name(), name());
        let seed = std::format!("{}/s{}", DIRS[rng.below(3) as usize], rng.below(4));
        // Errors (missing names, taken names) are part of the mix.
        let _ = match rng.below(4) {
            0 => fs.link(&seed, &to),
            1 => fs.link(&from, &to),
            2 => fs.unlink(&from),
            _ => fs.rename(&from, &to),
        };
        if rng.one_in(5) {
            fs.writeback().unwrap();
        }
    }
    drop(fs);
    let log = recorder.log.lock().unwrap()[start..].to_vec();
    let points: Vec<usize> = match cuts {
        Some(count) => (0..count)
            .map(|_| rng.below(log.len() as u64 + 1) as usize)
            .collect(),
        None => (0..=log.len()).collect(),
    };
    let (mut clean, mut leaks) = (0, 0);
    for point in points {
        let (image, _) = mount_and_settle(image_at(&base, &log, point));
        let what = std::format!("cut after write {point} of {} ({blocks} pages)", log.len());
        match judge(image, blocks, &what) {
            Verdict::Clean => clean += 1,
            _ => leaks += 1,
        }
    }
    (clean, leaks)
}

/// Random link/unlink/rename sequences with flushes and writebacks between
/// them, cut at random writes: every cut replays to what [`judge`] accepts.
#[test]
fn random_link_sequences_replay_clean() {
    let mut case = 0;
    for_seeds("random_link_sequences_replay_clean", |_, rng| {
        case += 1;
        if case > workload::cases(16) {
            return;
        }
        random_sequence(rng, Some(20));
    });
}

/// The CI seed that found a journaled 4-page cache committing in the middle
/// of a `link` (its count raised, its new name not yet logged): replayed at
/// every cut, the half-done link only leaks a count, which the repair
/// settles, and never leaves a count below its names.
#[test]
fn a_mid_operation_commit_of_a_tiny_cache_only_leaks() {
    let (clean, leaks) = random_sequence(&mut Rng::new(0xb071_afd9_303b_d51e), None);
    assert!(clean > 0, "no cut replayed clean");
    assert!(leaks > 0, "the seed no longer commits mid-operation");
}

#[test]
fn a_file_at_the_link_limit_takes_no_more_names() {
    use crate::layout::{le16, put16, INO_LINKS};
    let (io, fs) = fresh(2 << 20, 1024);
    put(&fs, "/f", 7);
    let ino = fs.lookup("/f").unwrap().ino as u32;
    // As if 31 999 other names existed: the count alone decides.
    let mut inode = fs.read_inode(ino).unwrap();
    put16(&mut inode, INO_LINKS, crate::MAX_LINKS);
    fs.write_inode(ino, &inode).unwrap();
    assert_eq!(fs.link("/f", "/g"), Err(crate::Ext2Error::TooManyLinks));
    assert!(fs.lookup("/g").is_err(), "a refused link left a name");
    assert_eq!(
        le16(&fs.read_inode(ino).unwrap(), INO_LINKS),
        crate::MAX_LINKS
    );
    // One name fewer, and a link fits again.
    put16(&mut inode, INO_LINKS, crate::MAX_LINKS - 1);
    fs.write_inode(ino, &inode).unwrap();
    fs.link("/f", "/g").unwrap();
    assert_eq!(fs.lookup("/g").unwrap().nlink, u32::from(crate::MAX_LINKS));
    put16(&mut inode, INO_LINKS, 2);
    fs.write_inode(ino, &inode).unwrap();
    fs.flush().unwrap();
    drop(fs);
    assert_clean(&io);
}
