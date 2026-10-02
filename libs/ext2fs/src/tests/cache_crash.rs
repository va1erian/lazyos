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
