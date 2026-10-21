//! Commits in pieces (`commit_piece.rs`, `cache/piecewise.rs`, issue #940).
//!
//! The host lets go of the volume between pieces, so other operations dirty,
//! allocate and free blocks while a commit is half written. The disk must
//! still only see a prefix of a consistent image in phase order: every prefix
//! of every write, with operations interleaved between any two requests, is
//! judged like `cache_crash`'s (never a reachable-but-free block, never one
//! file's bytes in another).

use std::cell::Cell;
use std::sync::Arc;

use fuzzkit::for_seeds;

use super::cache_crash::{image_at, judge, Recorder};
use super::workload::{self, Model};
use super::*;
use crate::{CacheConfig, Progress};

const CASES: usize = 40;
const POINTS: usize = 60;

/// Run one commit to its end, `between` operations after every request.
fn commit_in_pieces(fs: &Ext2, mut between: impl FnMut()) {
    loop {
        match fs.writeback_piece(&mut || true).unwrap() {
            Progress::Done => return,
            Progress::More => between(),
        }
    }
}

/// Operations (creates, rewrites, deletes, renames) between every two
/// requests of a commit, plain commits and syncs mixed in: every prefix of
/// the writes recovers without a forbidden problem, and the end state is the
/// model's.
#[test]
fn every_crash_point_of_an_interleaved_piecewise_commit_is_safe() {
    let mut case = 0;
    for_seeds(
        "every_crash_point_of_an_interleaved_piecewise_commit_is_safe",
        |_, rng| {
            case += 1;
            if case > workload::cases(CASES) {
                return;
            }
            let block_size = BLOCK_SIZES[rng.below(3) as usize] as usize;
            let disk = formatted(2 << 20, block_size as u32);
            let base = disk.snapshot();
            let recorder = Recorder {
                disk: disk.clone(),
                block_size,
                log: Arc::default(),
            };
            let blocks = [8, 16, 64, 256][rng.below(4) as usize];
            let fs =
                Ext2::open_cached(Box::new(recorder.clone()), clock, CacheConfig::heap(blocks))
                    .unwrap();
            let mut model = Model::default();
            let full = Cell::new(false);
            for _ in 0..rng.range(3, 8) {
                for _ in 0..rng.range(5, 25) {
                    if !workload::step(&[&fs], &mut model, rng) {
                        full.set(true);
                        break;
                    }
                }
                if full.get() {
                    break;
                }
                let mut busy = 0;
                commit_in_pieces(&fs, || {
                    let steps = rng.below(3);
                    for _ in 0..steps {
                        if !workload::step(&[&fs], &mut model, rng) {
                            full.set(true);
                            return;
                        }
                    }
                    busy += 1;
                    // A plain commit or a sync landing mid-way ends it.
                    if busy % 7 == 3 {
                        match rng.below(3) {
                            0 => fs.writeback().unwrap(),
                            1 => fs.flush().unwrap(),
                            _ => {}
                        }
                    }
                });
            }
            fs.flush().unwrap();
            // A full disk leaves the model a step ahead of the volume.
            if !full.get() {
                workload::verify(&fs, &model);
            }
            assert!(fsck(&disk.snapshot()).is_empty(), "the end state is clean");
            drop(fs);
            let log = recorder.log.lock().unwrap().clone();
            for _ in 0..POINTS {
                let point = rng.below(log.len() as u64 + 1) as usize;
                judge(image_at(&base, &log, point), point);
            }
        },
    );
}

/// Run to its end with nothing in between, a piecewise commit writes what
/// one commit does: same image, same free counters.
#[test]
fn an_undisturbed_piecewise_commit_matches_a_plain_one() {
    let mut case = 0;
    for_seeds(
        "an_undisturbed_piecewise_commit_matches_a_plain_one",
        |_, rng| {
            case += 1;
            if case > workload::cases(CASES / 2) {
                return;
            }
            let block_size = BLOCK_SIZES[rng.below(3) as usize];
            let (plain, pieces) = (
                formatted(2 << 20, block_size),
                formatted(2 << 20, block_size),
            );
            let open = |disk: &MemIo| {
                Ext2::open_cached(Box::new(disk.clone()), clock, CacheConfig::heap(256)).unwrap()
            };
            let (a, b) = (open(&plain), open(&pieces));
            let mut model = Model::default();
            for _ in 0..rng.range(10, 50) {
                if !workload::step(&[&a, &b], &mut model, rng) {
                    break;
                }
                if rng.below(8) == 0 {
                    a.writeback().unwrap();
                    commit_in_pieces(&b, || {});
                }
            }
            a.flush().unwrap();
            b.flush().unwrap();
            assert!(plain.snapshot() == pieces.snapshot(), "same bytes on disk");
            assert!(!b.commit_in_progress());
        },
    );
}

/// The shadow: a block the snapshot holds is changed before its piece, and
/// the piece must write the snapshot's version, not the new one, while the
/// new one still reaches the disk by the next commit.
#[test]
fn a_block_changed_before_its_piece_is_written_as_it_was() {
    let disk = formatted(2 << 20, 1024);
    let fs = Ext2::open_cached(Box::new(disk.clone()), clock, CacheConfig::heap(256)).unwrap();
    fs.write_file("/one", &[1u8; 3000], 0o644, 0, 0, 1).unwrap();
    // First request only; the rest of the snapshot is still pending.
    assert_eq!(fs.writeback_piece(&mut || true).unwrap(), Progress::More);
    // New file: new inode in the same table block, new data and bitmap bits.
    fs.write_file("/two", &[2u8; 5000], 0o644, 0, 0, 1).unwrap();
    commit_in_pieces(&fs, || {});
    // The commit ended with /two's blocks still dirty: it was not in the
    // snapshot, so a crash now loses it whole but never half of it.
    let mid = Ext2::open(Box::new(MemIo::from_bytes(disk.snapshot())), clock).unwrap();
    assert!(fsck(&disk.snapshot())
        .iter()
        .all(|problem| !super::cache_crash::FORBIDDEN
            .iter()
            .any(|bad| problem.contains(bad))));
    assert!(mid.read("/one", 0, &mut [0u8; 8]).is_ok());
    fs.flush().unwrap();
    assert!(fsck(&disk.snapshot()).is_empty());
    let end = Ext2::open(Box::new(MemIo::from_bytes(disk.snapshot())), clock).unwrap();
    assert!(super::cache_crash::holds(&end, "/two", &[2u8; 5000]));
    assert!(super::cache_crash::holds(&end, "/one", &[1u8; 3000]));
}
