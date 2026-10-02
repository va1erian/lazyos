//! Real crash damage, not hand-made: the disk is rebuilt from a prefix of the
//! block writes a cached volume issued (dirty blocks that never reached it
//! are simply lost), then `recover` must certify it. Every file reachable on
//! the crashed image keeps its bytes, and the checker passes afterwards. The
//! repair's own writes are cut the same way: a power cut during a repair
//! leaves a volume the next repair accepts.

use std::sync::Arc;

use fuzzkit::{for_seeds, Rng};

use super::cache_crash::{image_at, Recorder};
use super::repair::contents;
use super::workload::{self, Model};
use super::*;
use crate::{CacheConfig, Owner, Recovery, ORPHAN_PREFIX};

const CASES: usize = 40;
/// Crash points tried per case.
const POINTS: usize = 12;
/// Of those, how many also cut the repair itself short.
const REPAIR_CUTS: usize = 3;

/// A disk that logs its block writes, and a cached volume on it.
fn recorded(disk: &MemIo, block_size: usize, cache_blocks: usize) -> (Recorder, Ext2) {
    let recorder = Recorder {
        disk: disk.clone(),
        block_size,
        log: Arc::default(),
    };
    let config = CacheConfig::heap(cache_blocks);
    let fs = Ext2::open_cached(Box::new(recorder.clone()), clock, config).unwrap();
    (recorder, fs)
}

/// Recover `image` through a cache, flush, and check the result against
/// `before` (the files reachable on the crashed image). Returns the disk
/// and the log of the recovery's writes.
fn recover_and_judge(
    image: Vec<u8>,
    block_size: usize,
    before: &FileMap,
    what: &str,
) -> (MemIo, Recorder) {
    let disk = MemIo::from_bytes(image);
    let (recorder, mut fs) = recorded(&disk, block_size, 64);
    match fs.recover(ORPHAN_PREFIX).unwrap() {
        Recovery::WasClean | Recovery::Recovered { .. } => {}
        Recovery::StillUnclean(reason) => panic!("{what}: {reason}"),
    }
    fs.flush().unwrap();
    drop(fs);
    let problems = fsck(&disk.snapshot());
    assert!(problems.is_empty(), "{what}: {problems:#?}");
    let after = contents(&open(&disk));
    for (ino, data) in before {
        assert!(
            after.get(ino) == Some(data),
            "{what}: inode {ino} lost or changed"
        );
    }
    (disk, recorder)
}

type FileMap = std::collections::BTreeMap<u64, Vec<u8>>;

/// What is reachable on a crashed image.
fn reachable(image: &[u8]) -> FileMap {
    let io = MemIo::from_bytes(image.to_vec());
    io.set_writable(false);
    contents(&Ext2::open(Box::new(io), clock).unwrap())
}

/// Cut the repair of `image` at a few points: each cut must recover too.
fn cut_the_repair(image: &[u8], block_size: usize, before: &FileMap, rng: &mut Rng, what: &str) {
    let (_, recorder) = recover_and_judge(image.to_vec(), block_size, before, what);
    let log = recorder.log.lock().unwrap().clone();
    for _ in 0..REPAIR_CUTS {
        let cut = rng.below(log.len() as u64 + 1) as usize;
        let what = std::format!("{what}, repair cut after write {cut}");
        recover_and_judge(image_at(image, &log, cut), block_size, before, &what);
    }
}

#[test]
fn every_crash_point_is_recovered_without_losing_a_file() {
    let mut case = 0;
    for_seeds(
        "every_crash_point_is_recovered_without_losing_a_file",
        |seed, rng| {
            case += 1;
            if case > workload::cases(CASES) {
                return;
            }
            let block_size = BLOCK_SIZES[rng.below(3) as usize] as usize;
            let disk = formatted(2 << 20, block_size as u32);
            let base = disk.snapshot();
            let (recorder, fs) = recorded(&disk, block_size, [4, 16, 256][rng.below(3) as usize]);
            let mut model = Model::default();
            for _ in 0..rng.range(10, 60) {
                if !workload::step(&[&fs], &mut model, rng) {
                    break;
                }
                match rng.below(10) {
                    0 => fs.flush().unwrap(),
                    1 => fs.writeback().unwrap(),
                    _ => {}
                }
            }
            drop(fs);
            let log = recorder.log.lock().unwrap().clone();
            for index in 0..POINTS {
                let point = rng.below(log.len() as u64 + 1) as usize;
                let image = image_at(&base, &log, point);
                let before = reachable(&image);
                let what = std::format!("seed {seed:#x}, crash after write {point}");
                if index < REPAIR_CUTS {
                    cut_the_repair(&image, block_size, &before, rng, &what);
                } else {
                    recover_and_judge(image, block_size, &before, &what);
                }
            }
        },
    );
}

/// File and directory renames cut at every write: the shapes that leave a
/// directory under two names, or a `..` that disagrees with its one name.
#[test]
fn every_rename_crash_point_is_recovered() {
    for block_size in BLOCK_SIZES {
        let disk = formatted(2 << 20, block_size);
        let (recorder, fs) = recorded(&disk, block_size as usize, 64);
        for dir in ["/a", "/b", "/x", "/y"] {
            fs.mkdir(dir, 0o755, Owner::ROOT).unwrap();
        }
        for n in 0..60 {
            let name = std::format!("/a/padding-entry-with-a-long-name-{n:03}");
            fs.write_file(&name, b"pad", 0o644, 0, 0, 1).unwrap();
        }
        fs.write_file("/a/one", &[1u8; 3000], 0o644, 0, 0, 1)
            .unwrap();
        fs.write_file("/b/two", &[2u8; 5000], 0o644, 0, 0, 1)
            .unwrap();
        fs.write_file("/x/three", &[3u8; 700], 0o644, 0, 0, 1)
            .unwrap();
        fs.mkdir("/x/inner", 0o755, Owner::ROOT).unwrap();
        fs.flush().unwrap();
        let base = disk.snapshot();
        let start = recorder.log.lock().unwrap().len();
        fs.rename("/a/one", "/b/two").unwrap();
        fs.rename("/x", "/y/x").unwrap();
        fs.rename("/y/x/inner", "/a/inner").unwrap();
        fs.unlink("/a/padding-entry-with-a-long-name-007").unwrap();
        fs.rmdir("/a/inner").unwrap();
        fs.flush().unwrap();
        drop(fs);
        let log = recorder.log.lock().unwrap()[start..].to_vec();
        for point in 0..=log.len() {
            let image = image_at(&base, &log, point);
            let before = reachable(&image);
            let what = std::format!("{block_size}: crash after write {point}");
            recover_and_judge(image, block_size as usize, &before, &what);
        }
    }
}
