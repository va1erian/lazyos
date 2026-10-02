//! Power cuts against a cached volume: the disk is rebuilt from every prefix
//! of the block writes the cache issued, and each such image must keep the
//! promises of `docs/architecture/block-cache.md`:
//!
//! * at a completed `flush` the image is exactly what was flushed, and clean;
//! * anywhere else the checker may find leaks, stale counters, link counts
//!   and `i_blocks` out of step, or an entry naming an inode whose
//!   initialisation was lost, but never a block or inode that is reachable
//!   and free, a block claimed twice, or a garbled directory;
//! * no file ever shows bytes that belonged to another file (each workload
//!   file is filled with its own tag byte).

use std::sync::{Arc, Mutex};

use fuzzkit::for_seeds;

use super::workload::{self, Model};
use super::*;
use crate::{BlockIo, CacheConfig, IoError, SECTOR_SIZE};

/// Problems the documented crash semantics never allow.
const FORBIDDEN: [&str; 8] = [
    "is in use but marked free",
    "is reachable but marked free",
    "claimed by inodes",
    "claims metadata block",
    "bad directory record",
    "past the table",
    "out of range",
    "doubly used",
];

/// The checker's complaints about free counters and directory counts.
const COUNTERS: [&str; 4] = ["free_blocks", "free_inodes", "used_dirs", "superblock free"];

const CASES: usize = 40;
/// Crash points tried per case, besides every completed flush.
const POINTS: usize = 40;

/// Every block written, in order, as (first sector, bytes).
type BlockLog = Arc<Mutex<Vec<(u64, Vec<u8>)>>>;

/// A disk that remembers every block it was asked to write, in order.
#[derive(Clone)]
struct Recorder {
    disk: MemIo,
    block_size: usize,
    log: BlockLog,
}

impl BlockIo for Recorder {
    fn sector_count(&self) -> u64 {
        self.disk.sector_count()
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), IoError> {
        self.disk.read_sectors(lba, buf)
    }

    fn read_sectors_vectored(&self, lba: u64, bufs: &mut [&mut [u8]]) -> Result<(), IoError> {
        self.disk.read_sectors_vectored(lba, bufs)
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), IoError> {
        self.disk.write_sectors(lba, buf)?;
        let sectors = (self.block_size / SECTOR_SIZE) as u64;
        let mut log = self.log.lock().unwrap();
        for (index, block) in buf.chunks(self.block_size).enumerate() {
            log.push((lba + index as u64 * sectors, block.to_vec()));
        }
        Ok(())
    }

    fn write_sectors_vectored(&self, lba: u64, bufs: &[&[u8]]) -> Result<(), IoError> {
        self.write_sectors(lba, &bufs.concat())
    }

    fn flush(&self) -> Result<(), IoError> {
        self.disk.flush()
    }

    fn is_writable(&self) -> bool {
        self.disk.is_writable()
    }
}

/// `base` with the first `count` logged block writes applied.
fn image_at(base: &[u8], log: &[(u64, Vec<u8>)], count: usize) -> Vec<u8> {
    let mut image = base.to_vec();
    for (lba, block) in &log[..count] {
        let at = *lba as usize * SECTOR_SIZE;
        image[at..at + block.len()].copy_from_slice(block);
    }
    image
}

/// Judge one crashed image against the documented semantics.
fn judge(image: Vec<u8>, point: usize) {
    let problems = fsck(&image);
    let forbidden: Vec<_> = problems
        .iter()
        .filter(|problem| FORBIDDEN.iter().any(|bad| problem.contains(bad)))
        .collect();
    assert!(
        forbidden.is_empty(),
        "crash after write {point}: {forbidden:#?}"
    );
    let io = MemIo::from_bytes(image);
    io.set_writable(false);
    let fs = Ext2::open(Box::new(io), clock).expect("a crashed image still mounts");
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

#[test]
fn every_crash_point_keeps_the_documented_semantics() {
    let mut case = 0;
    for_seeds(
        "every_crash_point_keeps_the_documented_semantics",
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
            let blocks = [4, 16, 256][rng.below(3) as usize];
            let config = CacheConfig::heap(blocks);
            let fs = Ext2::open_cached(Box::new(recorder.clone()), clock, config).unwrap();
            let mut model = Model::default();
            // (log length, image) at each completed flush.
            let mut durable = Vec::new();
            for _ in 0..rng.range(10, 60) {
                if !workload::step(&[&fs], &mut model, rng) {
                    break;
                }
                match rng.below(10) {
                    0 => {
                        fs.flush().unwrap();
                        durable.push((recorder.log.lock().unwrap().len(), disk.snapshot()));
                    }
                    1 => fs.writeback().unwrap(),
                    _ => {}
                }
            }
            drop(fs);
            let log = recorder.log.lock().unwrap().clone();
            for (count, snapshot) in &durable {
                let image = image_at(&base, &log, *count);
                assert!(image == *snapshot, "a completed flush is the whole truth");
                assert!(fsck(&image).is_empty(), "flushed images are clean");
            }
            for _ in 0..POINTS {
                let point = rng.below(log.len() as u64 + 1) as usize;
                judge(image_at(&base, &log, point), point);
            }
        },
    );
}

/// Whether `path` on `fs` holds exactly `data`.
fn holds(fs: &Ext2, path: &str, data: &[u8]) -> bool {
    let mut back = std::vec![0u8; data.len() + 1];
    fs.read(path, 0, &mut back) == Ok(data.len()) && back[..data.len()] == *data
}

/// Renames keep the direct driver's promise through the cache: at every
/// crash point a renamed file (replacing another or not, across
/// directories) and a moved directory are reachable under the old name or
/// the new one, never under neither.
#[test]
fn a_crash_never_loses_a_renamed_file_or_directory() {
    for block_size in BLOCK_SIZES {
        let disk = formatted(2 << 20, block_size);
        let recorder = Recorder {
            disk: disk.clone(),
            block_size: block_size as usize,
            log: Arc::default(),
        };
        let fs =
            Ext2::open_cached(Box::new(recorder.clone()), clock, CacheConfig::heap(64)).unwrap();
        for dir in ["/a", "/b", "/x", "/y"] {
            fs.mkdir(dir, 0o755, crate::Owner::ROOT).unwrap();
        }
        // Enough entries that the names live in several directory blocks.
        for n in 0..60 {
            let name = std::format!("/a/padding-entry-with-a-long-name-{n:03}");
            fs.write_file(&name, b"pad", 0o644, 0, 0, 1).unwrap();
        }
        let (one, two, three) = ([1u8; 3000], [2u8; 5000], [3u8; 700]);
        fs.write_file("/a/one", &one, 0o644, 0, 0, 1).unwrap();
        fs.write_file("/b/two", &two, 0o644, 0, 0, 1).unwrap();
        fs.write_file("/x/three", &three, 0o644, 0, 0, 1).unwrap();
        fs.flush().unwrap();
        let base = disk.snapshot();
        let start = recorder.log.lock().unwrap().len();
        fs.rename("/a/one", "/b/two").unwrap(); // replaces two
        fs.rename("/b/two", "/a/moved-back").unwrap(); // a fresh name
        fs.rename("/x", "/y/x").unwrap(); // a directory
        fs.flush().unwrap();
        drop(fs);
        let log = recorder.log.lock().unwrap()[start..].to_vec();
        for point in 0..=log.len() {
            let io = MemIo::from_bytes(image_at(&base, &log, point));
            io.set_writable(false);
            let fs = Ext2::open(Box::new(io), clock).unwrap();
            let found = ["/a/one", "/b/two", "/a/moved-back"]
                .iter()
                .any(|path| holds(&fs, path, &one));
            assert!(
                found,
                "{block_size}: crash after write {point} lost the renamed file"
            );
            let moved = ["/x/three", "/y/x/three"]
                .iter()
                .any(|path| holds(&fs, path, &three));
            assert!(
                moved,
                "{block_size}: crash after write {point} lost the moved directory"
            );
        }
    }
}

/// Deleting a parked orphan stays resumable through the cache: whatever
/// prefix of its writes reaches the disk, the next mount's reclaim finishes
/// the delete and leaves a volume with no leak at all.
#[test]
fn a_parked_delete_can_always_be_resumed() {
    for block_size in BLOCK_SIZES {
        let disk = formatted(2 << 20, block_size);
        let recorder = Recorder {
            disk: disk.clone(),
            block_size: block_size as usize,
            log: Arc::default(),
        };
        let fs =
            Ext2::open_cached(Box::new(recorder.clone()), clock, CacheConfig::heap(32)).unwrap();
        fs.mkdir("/d", 0o755, crate::Owner::ROOT).unwrap();
        // Big enough for double-indirect blocks with 1 KiB blocks.
        fs.write_file("/d/.unlinked-7", &[9u8; 300_000], 0o644, 0, 0, 1)
            .unwrap();
        fs.write_file("/d/keep", &[4u8; 9_000], 0o644, 0, 0, 1)
            .unwrap();
        fs.flush().unwrap();
        let base = disk.snapshot();
        let start = recorder.log.lock().unwrap().len();
        fs.unlink_parked("/d/.unlinked-7").unwrap();
        fs.writeback().unwrap();
        drop(fs);
        let log = recorder.log.lock().unwrap()[start..].to_vec();
        for point in 0..=log.len() {
            let io = MemIo::from_bytes(image_at(&base, &log, point));
            let fs = Ext2::open(Box::new(io.clone()), clock).unwrap();
            let report = fs.reclaim_orphans(".unlinked-");
            assert!(
                report.failed.is_empty(),
                "{block_size}/{point}: {:?}",
                report.failed
            );
            fs.flush().unwrap();
            assert!(holds(&fs, "/d/keep", &[4u8; 9_000]));
            drop(fs);
            // Nothing recomputes the free counters at mount (with or without
            // the cache), so judge by the bitmaps: no leak, nothing else.
            let problems: Vec<_> = fsck(&io.snapshot())
                .into_iter()
                .filter(|problem| !COUNTERS.iter().any(|counter| problem.contains(counter)))
                .collect();
            assert!(
                problems.is_empty(),
                "{block_size}: after write {point}: {problems:#?}"
            );
        }
    }
}
