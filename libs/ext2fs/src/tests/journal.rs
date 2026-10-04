//! The journal: creation, round trips, replay after a power cut at every
//! write the cache issued, failure injection, and the log format itself.
//!
//! Unlike the unjournaled crash tests, the bar after a crash is absolute: a
//! mount replays the log and the independent checker finds *nothing*, because
//! every commit is one atomic transaction taken at an operation boundary.

use std::sync::Arc;

use fuzzkit::for_seeds;

use super::cache_crash::{image_at, Recorder};
use super::workload::{self, Model};
use super::*;
use crate::journal::MIN_JOURNAL_BLOCKS;
use crate::{CacheConfig, Ext2Error, Owner};

const CASES: usize = 24;
const POINTS: usize = 60;
const JOURNAL_BLOCKS: u32 = 128;

/// A formatted image of `bytes` that already has a journal.
fn journaled(bytes: u64, block_size: u32) -> MemIo {
    let io = formatted(bytes, block_size);
    let fs = open(&io);
    fs.add_journal(JOURNAL_BLOCKS).expect("add journal");
    drop(fs);
    io
}

/// Mount `image` writable (replaying its log), return what was found, and
/// leave the replayed bytes behind.
fn mount_and_settle(image: Vec<u8>) -> (Vec<u8>, bool) {
    let io = MemIo::from_bytes(image);
    let fs = open_cached(&io, 64);
    let recovered = fs.journal_recovered();
    fs.flush().unwrap();
    drop(fs);
    (io.snapshot(), recovered)
}

#[test]
fn a_new_journal_leaves_a_clean_volume() {
    for block_size in BLOCK_SIZES {
        let io = formatted(2 << 20, block_size);
        let fs = open(&io);
        assert!(!fs.has_journal());
        assert_eq!(
            fs.add_journal(MIN_JOURNAL_BLOCKS - 1),
            Err(Ext2Error::Invalid)
        );
        assert_eq!(fs.add_journal(u32::MAX), Err(Ext2Error::Invalid));
        fs.add_journal(JOURNAL_BLOCKS).unwrap();
        drop(fs);
        assert_clean(&io);

        let fs = open(&io);
        assert!(fs.has_journal());
        assert!(fs.was_clean_at_mount());
        assert_eq!(fs.add_journal(JOURNAL_BLOCKS), Err(Ext2Error::Exists));
    }
}

#[test]
fn a_journal_too_big_for_the_volume_is_refused_untouched() {
    let io = formatted(1 << 20, 1024);
    let before = io.snapshot();
    let fs = open(&io);
    assert_eq!(fs.add_journal(2000), Err(Ext2Error::Invalid));
    drop(fs);
    assert!(io.snapshot() == before, "a refused journal writes nothing");
}

#[test]
fn a_journaled_volume_works_like_any_other() {
    for block_size in BLOCK_SIZES {
        let io = journaled(4 << 20, block_size);
        let fs = open_cached(&io, 128);
        let mut model = Model::default();
        for_seeds("a_journaled_volume_works_like_any_other", |_, rng| {
            for _ in 0..40 {
                if !workload::step(&[&fs], &mut model, rng) {
                    break;
                }
                if rng.one_in(6) {
                    fs.flush().unwrap();
                }
            }
        });
        workload::verify(&fs, &model);
        fs.flush().unwrap();
        drop(fs);
        assert_clean(&io);
        let fs = open_cached(&io, 128);
        assert!(
            !fs.journal_recovered(),
            "a clean stop leaves nothing to replay"
        );
        workload::verify(&fs, &model);
    }
}

#[test]
fn a_tiny_cache_still_commits_consistently() {
    // With four pages every operation fills the cache with metadata and
    // forces a commit mid-operation; the volume must still end up clean.
    let io = journaled(4 << 20, 1024);
    let fs = open_cached(&io, 4);
    let mut model = Model::default();
    for_seeds("a_tiny_cache_still_commits_consistently", |_, rng| {
        for _ in 0..30 {
            if !workload::step(&[&fs], &mut model, rng) {
                break;
            }
        }
    });
    workload::verify(&fs, &model);
    fs.flush().unwrap();
    drop(fs);
    assert_clean(&io);
}

#[test]
fn every_crash_point_replays_to_a_clean_volume() {
    let mut case = 0;
    let mut replayed = 0;
    for_seeds("every_crash_point_replays_to_a_clean_volume", |_, rng| {
        case += 1;
        if case > workload::cases(CASES) {
            return;
        }
        let block_size = BLOCK_SIZES[rng.below(3) as usize] as usize;
        let disk = journaled(2 << 20, block_size as u32);
        let base = disk.snapshot();
        let recorder = Recorder {
            disk: disk.clone(),
            block_size,
            log: Arc::default(),
        };
        let blocks = [64, 128, 256][rng.below(3) as usize];
        let fs = Ext2::open_cached(Box::new(recorder.clone()), clock, CacheConfig::heap(blocks))
            .unwrap();
        let mut model = Model::default();
        for _ in 0..rng.range(10, 50) {
            if !workload::step(&[&fs], &mut model, rng) {
                break;
            }
            match rng.below(8) {
                0 => fs.flush().unwrap(),
                1 => fs.writeback().unwrap(),
                _ => {}
            }
        }
        drop(fs);
        let log = recorder.log.lock().unwrap().clone();
        for _ in 0..POINTS {
            let point = rng.below(log.len() as u64 + 1) as usize;
            let (image, recovered) = mount_and_settle(image_at(&base, &log, point));
            replayed += usize::from(recovered);
            let problems = fsck(&image);
            assert!(
                problems.is_empty(),
                "crash after write {point}: {problems:#?}"
            );
            // No file may show another file's bytes (each carries its tag).
            let io = MemIo::from_bytes(image);
            let fs = open(&io);
            workload::walk(&fs, &mut |path, data| {
                let mut tags = data.iter().filter(|&&byte| byte != 0);
                if let Some(&tag) = tags.next() {
                    assert!(
                        tags.all(|&byte| byte == tag),
                        "crash after write {point}: {path} shows another file's bytes"
                    );
                }
            });
        }
    });
    assert!(replayed > 0, "no crash point exercised the replay");
}

#[test]
fn a_flush_is_durable_without_any_replay() {
    let io = journaled(2 << 20, 4096);
    let fs = open_cached(&io, 64);
    fs.mkdir("/d", 0o755, Owner::ROOT).unwrap();
    fs.create("/d/f", 0o644, Owner::ROOT).unwrap();
    fs.write("/d/f", 0, &[7u8; 10_000]).unwrap();
    fs.flush().unwrap();
    // Power cut: the volume is dropped without another write.
    let image = io.snapshot();
    std::mem::forget(fs);
    assert!(fsck(&image).is_empty());
    let (_, recovered) = mount_and_settle(image);
    assert!(!recovered, "a clean sync leaves an empty log");
}

#[test]
fn a_committed_transaction_is_replayed_and_an_unfinished_one_is_not() {
    let io = journaled(2 << 20, 1024);
    let base = io.snapshot();
    let recorder = Recorder {
        disk: io.clone(),
        block_size: 1024,
        log: Arc::default(),
    };
    let fs = Ext2::open_cached(Box::new(recorder.clone()), clock, CacheConfig::heap(64)).unwrap();
    fs.mkdir("/kept", 0o755, Owner::ROOT).unwrap();
    fs.flush().unwrap();
    let after_first = recorder.log.lock().unwrap().len();
    fs.mkdir("/second", 0o755, Owner::ROOT).unwrap();
    fs.flush().unwrap();
    drop(fs);
    let log = recorder.log.lock().unwrap().clone();

    // Every cut inside the second commit leaves "/kept"; "/second" appears
    // exactly when the commit block (and so the whole transaction) landed.
    let mut seen_second = false;
    for point in after_first..=log.len() {
        let (image, _) = mount_and_settle(image_at(&base, &log, point));
        assert!(fsck(&image).is_empty(), "cut at {point}");
        let fs = open(&MemIo::from_bytes(image));
        assert!(fs.lookup("/kept").is_ok(), "cut at {point}");
        let has_second = fs.lookup("/second").is_ok();
        assert!(
            has_second || !seen_second,
            "cut at {point}: the directory came back lost"
        );
        seen_second |= has_second;
    }
    assert!(seen_second, "the finished commit shows its directory");
}

#[test]
fn a_block_that_looks_like_the_journal_magic_is_escaped() {
    let io = journaled(2 << 20, 1024);
    let fs = open_cached(&io, 64);
    let mut journal = fs.load_journal().unwrap();
    let mut block = std::vec![0x5Au8; 1024];
    block[..4].copy_from_slice(&0xC03B_3998u32.to_be_bytes());
    // A free block far from the metadata: nothing else owns it.
    let target = 1500u64;
    let geometry = crate::journal::commit::Geometry {
        block_size: 1024,
        sectors_per_block: 2,
        max_run: 64,
    };
    // Commit a transaction and "crash" before the checkpoint: no finish().
    journal
        .commit(&io, &geometry, 0, &[(target, &block)])
        .unwrap();
    std::mem::forget(fs);
    let mut image = io.snapshot();
    // Pre-replay, the target is untouched.
    assert!(image[target as usize * 1024..][..1024]
        .iter()
        .all(|&b| b == 0));
    let (settled, recovered) = mount_and_settle(std::mem::take(&mut image));
    assert!(recovered);
    assert!(settled[target as usize * 1024..][..1024] == block[..]);
}

#[test]
fn a_hostile_log_is_refused_or_ignored_never_a_panic() {
    for_seeds("a_hostile_log_is_refused_or_ignored", |_, rng| {
        let io = journaled(1 << 20, 1024);
        let fs = open_cached(&io, 32);
        let journal = fs.load_journal().unwrap();
        drop(fs);
        let first = journal.blocks[0] as usize;
        // Arm the log and fill it with noise.
        io.with_bytes(|bytes| {
            let sb = &mut bytes[first * 1024..(first + 1) * 1024];
            sb[0x1C..0x20].copy_from_slice(&1u32.to_be_bytes());
            for &block in &journal.blocks[1..] {
                let at = block as usize * 1024;
                for byte in &mut bytes[at..at + 1024] {
                    *byte = rng.below(256) as u8;
                }
                if rng.one_in(2) {
                    // A plausible header with a random type and sequence.
                    bytes[at..at + 4].copy_from_slice(&0xC03B_3998u32.to_be_bytes());
                    bytes[at + 4..at + 8].copy_from_slice(&(rng.below(6) as u32).to_be_bytes());
                    bytes[at + 8..at + 12].copy_from_slice(&1u32.to_be_bytes());
                }
            }
        });
        let _ = Ext2::open(Box::new(io.clone()), clock);
    });
}

#[test]
fn a_failing_disk_never_leaves_a_volume_that_needs_more_than_a_replay() {
    // Kill the disk after n sector writes, flush, then bring the disk back:
    // whatever the failure interrupted, the next mount replays to clean.
    for n in (0..400).step_by(7) {
        let io = journaled(2 << 20, 1024);
        let fs = open_cached(&io, 64);
        fs.mkdir("/a", 0o755, Owner::ROOT).unwrap();
        fs.create("/a/f", 0o644, Owner::ROOT).unwrap();
        fs.write("/a/f", 0, &[3u8; 20_000]).unwrap();
        fs.flush().unwrap();
        io.fail_writes_after(n);
        let _ = fs.mkdir("/a/sub", 0o755, Owner::ROOT);
        let _ = fs.write("/a/f", 20_000, &[4u8; 9_000]);
        let _ = fs.flush();
        let image = io.snapshot();
        std::mem::forget(fs);
        let (settled, _) = mount_and_settle(image);
        let problems = fsck(&settled);
        assert!(
            problems.is_empty(),
            "disk died after {n} sectors: {problems:#?}"
        );
    }
}

#[test]
fn a_read_only_mount_shows_the_committed_state_and_writes_nothing() {
    let io = journaled(2 << 20, 1024);
    let base = io.snapshot();
    let recorder = Recorder {
        disk: io.clone(),
        block_size: 1024,
        log: Arc::default(),
    };
    let fs = Ext2::open_cached(Box::new(recorder.clone()), clock, CacheConfig::heap(64)).unwrap();
    fs.mkdir("/kept", 0o755, Owner::ROOT).unwrap();
    fs.flush().unwrap();
    let after_first = recorder.log.lock().unwrap().len();
    fs.mkdir("/second", 0o755, Owner::ROOT).unwrap();
    fs.flush().unwrap();
    drop(fs);
    let log = recorder.log.lock().unwrap().clone();

    let mut replayed_views = 0;
    for point in after_first..=log.len() {
        let image = image_at(&base, &log, point);
        // What a writable mount would show.
        let (settled, recovered) = mount_and_settle(image.clone());
        let writable = open(&MemIo::from_bytes(settled));
        // The same cut, mounted from a device that refuses writes.
        let device = MemIo::from_bytes(image.clone());
        device.set_writable(false);
        let readonly = Ext2::open(Box::new(device.clone()), clock).unwrap();
        assert!(readonly.is_read_only());
        assert!(
            device.snapshot() == image,
            "cut at {point}: the device was written"
        );
        for path in ["/kept", "/second"] {
            assert_eq!(
                readonly.lookup(path).is_ok(),
                writable.lookup(path).is_ok(),
                "cut at {point}: {path}"
            );
        }
        assert_eq!(readonly.journal_recovered(), recovered, "cut at {point}");
        replayed_views += usize::from(recovered);
        assert_eq!(
            readonly.mkdir("/nope", 0o755, Owner::ROOT),
            Err(Ext2Error::ReadOnly)
        );
    }
    assert!(replayed_views > 0, "no cut exercised a pending log");
}
