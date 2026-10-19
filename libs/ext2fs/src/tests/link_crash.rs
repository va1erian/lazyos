//! Hard links under power cuts: the update plan stages a release by
//! hard-linking unchanged files, and may do so on a volume that was not
//! cleanly unmounted. That is only sound if no crash prefix of `link`,
//! `unlink` or `rename` leaves an inode's link count *below* the number of
//! names that reach it (a later unlink would then free an inode another name
//! still uses). A count that is too high only leaks.
//!
//! Each scenario cuts the disk at every write prefix (uncached, and cached
//! with a tiny and a roomy cache), classifies the image with the independent
//! checker and asserts the verdict is never [`Verdict::TooLow`].

use std::string::String;
use std::sync::{Arc, Mutex};

use fuzzkit::for_seeds;

use super::cache_crash::{holds, image_at, Recorder, FORBIDDEN};
use super::workload;
use super::*;
use crate::{CacheConfig, Owner};

const DATA: [u8; 3000] = [7; 3000];

/// What a crashed image's link counts say.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Clean,
    /// Counts too high or unreachable inodes: space lost, nothing unsafe.
    Leak,
    /// A count below its names, a dangling entry or any other unsafe state.
    TooLow(String),
}

fn classify(image: &[u8]) -> Verdict {
    let problems = fsck(image);
    if problems.is_empty() {
        return Verdict::Clean;
    }
    for problem in &problems {
        let low = problem.split_once(": links ").is_some_and(|(_, rest)| {
            let mut numbers = rest.split(", entries ");
            let have = numbers.next().and_then(|n| n.parse::<u32>().ok());
            let entries = numbers.next().and_then(|n| n.parse::<u32>().ok());
            matches!((have, entries), (Some(h), Some(e)) if h < e)
        });
        if low || FORBIDDEN.iter().any(|bad| problem.contains(bad)) {
            return Verdict::TooLow(problem.clone());
        }
    }
    Verdict::Leak
}

fn root() -> Owner {
    Owner::ROOT
}

fn put(fs: &Ext2, path: &str, byte: u8) {
    fs.write_file(path, &[byte; 3000], 0o644, 0, 0, 1).unwrap();
}

/// Run `op` against the volume `setup` built, cut at every write prefix, and
/// return how many prefixes were clean and how many only leaked. `after`
/// checks a mounted crashed image (it may unlink and read).
fn sweep(
    setup: fn(&Ext2),
    op: fn(&Ext2),
    after: fn(&Ext2, usize),
    cache: Option<usize>,
) -> (usize, usize) {
    let block_size = 1024;
    let disk = formatted(2 << 20, block_size);
    let recorder = Recorder {
        disk: disk.clone(),
        block_size: block_size as usize,
        log: Arc::default(),
    };
    let fs = match cache {
        Some(blocks) => {
            Ext2::open_cached(Box::new(recorder.clone()), clock, CacheConfig::heap(blocks)).unwrap()
        }
        None => Ext2::open(Box::new(recorder.clone()), clock).unwrap(),
    };
    setup(&fs);
    fs.flush().unwrap();
    let base = disk.snapshot();
    let start = recorder.log.lock().unwrap().len();
    op(&fs);
    fs.flush().unwrap();
    drop(fs);
    let log = recorder.log.lock().unwrap()[start..].to_vec();
    let (mut clean, mut leaks) = (0, 0);
    for point in 0..=log.len() {
        let image = image_at(&base, &log, point);
        match classify(&image) {
            Verdict::Clean => clean += 1,
            Verdict::Leak => leaks += 1,
            Verdict::TooLow(why) => panic!("{cache:?}: cut after write {point}: {why}"),
        }
        let fs = Ext2::open(Box::new(MemIo::from_bytes(image)), clock).unwrap();
        after(&fs, point);
    }
    assert_eq!(classify(&image_at(&base, &log, log.len())), Verdict::Clean);
    (clean, leaks)
}

fn every_cache(setup: fn(&Ext2), op: fn(&Ext2), after: fn(&Ext2, usize)) {
    for cache in [None, Some(4), Some(64)] {
        sweep(setup, op, after, cache);
    }
}

/// On a mounted crashed image, drop one name of the shared file, and
/// whatever other names exist must still read the data.
fn drop_one_keep_other(fs: &Ext2, names: &[&str], point: usize) {
    let live: Vec<_> = names.iter().filter(|n| fs.lookup(n).is_ok()).collect();
    if live.len() < 2 {
        return;
    }
    fs.unlink(live[0]).unwrap();
    for name in &live[1..] {
        assert!(holds(fs, name, &DATA), "cut {point}: {name} lost its data");
    }
}

fn mkdirs(fs: &Ext2, dirs: &[&str]) {
    for dir in dirs {
        fs.mkdir(dir, 0o755, root()).unwrap();
    }
}

#[test]
fn link_into_a_new_directory() {
    every_cache(
        |fs| {
            mkdirs(fs, &["/a", "/b"]);
            put(fs, "/a/f", 7);
        },
        |fs| fs.link("/a/f", "/b/g").unwrap(),
        |fs, point| drop_one_keep_other(fs, &["/a/f", "/b/g"], point),
    );
}

#[test]
fn unlink_one_of_two_names() {
    every_cache(
        |fs| {
            mkdirs(fs, &["/a", "/b"]);
            put(fs, "/a/f", 7);
            fs.link("/a/f", "/b/g").unwrap();
        },
        |fs| fs.unlink("/a/f").unwrap(),
        |fs, point| drop_one_keep_other(fs, &["/a/f", "/b/g"], point),
    );
}

#[test]
fn rename_over_a_name_whose_inode_has_two_links() {
    every_cache(
        |fs| {
            mkdirs(fs, &["/a", "/b"]);
            put(fs, "/a/f", 7);
            fs.link("/a/f", "/b/g").unwrap();
            put(fs, "/a/t", 9);
        },
        |fs| fs.rename("/a/t", "/a/f").unwrap(),
        |fs, point| {
            // `/b/g` must keep the old data however the cut fell.
            assert!(
                holds(fs, "/b/g", &DATA) || fs.lookup("/b/g").is_err(),
                "cut {point}"
            );
        },
    );
}

#[test]
fn rename_a_file_that_has_two_links() {
    every_cache(
        |fs| {
            mkdirs(fs, &["/a", "/b"]);
            put(fs, "/a/f", 7);
            fs.link("/a/f", "/b/g").unwrap();
        },
        |fs| fs.rename("/a/f", "/b/h").unwrap(),
        |fs, point| drop_one_keep_other(fs, &["/a/f", "/b/g", "/b/h"], point),
    );
}

#[test]
fn rename_a_directory_holding_linked_files() {
    every_cache(
        |fs| {
            mkdirs(fs, &["/d", "/e", "/f"]);
            put(fs, "/d/p", 7);
            fs.link("/d/p", "/e/p2").unwrap();
            fs.link("/d/p", "/d/p3").unwrap();
        },
        |fs| {
            fs.rename("/d", "/f/d").unwrap(); // across parents
            fs.rename("/e", "/f/e").unwrap();
        },
        |fs, point| {
            // A directory mid-move has two names for one inode; judge the
            // hard-linked files only once each directory has one.
            let twins = [("/d", "/f/d"), ("/e", "/f/e")];
            if twins
                .iter()
                .any(|(old, new)| fs.lookup(old).is_ok() && fs.lookup(new).is_ok())
            {
                return;
            }
            let names = ["/d/p", "/d/p3", "/e/p2", "/f/d/p", "/f/d/p3", "/f/e/p2"];
            drop_one_keep_other(fs, &names, point);
        },
    );
}

#[test]
fn a_completed_sequence_leaves_nothing_behind() {
    // The whole story in one go: after every operation and a flush the image
    // is clean, with the link count equal to the names.
    for cache in [None, Some(8)] {
        let disk = formatted(2 << 20, 1024);
        let fs = match cache {
            Some(blocks) => open_cached(&disk, blocks),
            None => open(&disk),
        };
        mkdirs(&fs, &["/a", "/b"]);
        put(&fs, "/a/f", 7);
        fs.link("/a/f", "/b/g").unwrap();
        assert_eq!(fs.link_count("/a/f"), Ok(2));
        assert_eq!(fs.link("/a/f", "/b/g"), Err(crate::Ext2Error::Exists));
        assert_eq!(fs.link("/a", "/b/d"), Err(crate::Ext2Error::IsDir));
        fs.unlink("/a/f").unwrap();
        assert_eq!(fs.link_count("/b/g"), Ok(1));
        fs.flush().unwrap();
        assert_clean(&disk);
    }
}

/// Random link/unlink/rename/rmdir sequences over files made before the cut
/// window, each cut at random points, uncached or cached.
#[test]
fn random_link_sequences_never_drop_a_count_below_its_names() {
    let mut case = 0;
    for_seeds(
        "random_link_sequences_never_drop_a_count_below_its_names",
        |_, rng| {
            case += 1;
            if case > workload::cases(60) {
                return;
            }
            let disk = formatted(2 << 20, [1024, 2048][rng.below(2) as usize]);
            let block_size = disk_block_size(&disk);
            let recorder = Recorder {
                disk: disk.clone(),
                block_size,
                log: Arc::new(Mutex::new(Vec::new())),
            };
            let fs = match rng.below(3) {
                0 => Ext2::open(Box::new(recorder.clone()), clock).unwrap(),
                n => Ext2::open_cached(
                    Box::new(recorder.clone()),
                    clock,
                    CacheConfig::heap(if n == 1 { 4 } else { 64 }),
                )
                .unwrap(),
            };
            const DIRS: [&str; 4] = ["/a", "/b", "/c", "/a/x"];
            mkdirs(&fs, &DIRS);
            let mut files = Vec::new();
            for n in 0..6 {
                let path = std::format!("{}/s{n}", DIRS[rng.below(4) as usize]);
                put(&fs, &path, n as u8 + 1);
                files.push(path);
            }
            fs.flush().unwrap();
            let base = disk.snapshot();
            let start = recorder.log.lock().unwrap().len();
            for _ in 0..rng.range(8, 40) {
                let name = |rng: &mut fuzzkit::Rng| {
                    std::format!("{}/n{}", DIRS[rng.below(4) as usize], rng.below(5))
                };
                let known = |rng: &mut fuzzkit::Rng| {
                    let all = [files.clone(), std::vec![name(rng)]].concat();
                    all[rng.below(all.len() as u64) as usize].clone()
                };
                // Errors (missing names, full directories) are part of the mix.
                let op = match rng.below(8) {
                    0..=2 => ("link", known(rng), name(rng)),
                    3 => ("unlink", known(rng), String::new()),
                    4..=5 => ("rename", known(rng), name(rng)),
                    6 => ("rename", DIRS[rng.below(4) as usize].into(), name(rng)),
                    _ => ("rmdir", DIRS[rng.below(4) as usize].into(), String::new()),
                };
                let _ = match op.0 {
                    "link" => fs.link(&op.1, &op.2),
                    "unlink" => fs.unlink(&op.1),
                    "rename" => fs.rename(&op.1, &op.2),
                    _ => fs.rmdir(&op.1),
                };
                match rng.below(8) {
                    0 => fs.flush().unwrap(),
                    1 => fs.writeback().unwrap(),
                    _ => {}
                }
            }
            drop(fs);
            let log = recorder.log.lock().unwrap()[start..].to_vec();
            for _ in 0..30 {
                let point = rng.below(log.len() as u64 + 1) as usize;
                if let Verdict::TooLow(why) = classify(&image_at(&base, &log, point)) {
                    panic!("cut after write {point} of {}: {why}", log.len());
                }
            }
        },
    );
}

fn disk_block_size(disk: &MemIo) -> usize {
    // The superblock's `s_log_block_size` is at byte 1024 + 24.
    let image = disk.snapshot();
    1024 << u32::from_le_bytes(image[1048..1052].try_into().unwrap())
}
