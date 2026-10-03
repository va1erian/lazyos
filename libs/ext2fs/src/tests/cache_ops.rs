//! The write-back cache is invisible: a cached volume answers and ends up on
//! disk exactly like a direct one, and a failing disk is reported, not hidden.

use fuzzkit::{for_seeds, Rng};

use super::workload::{self, Model};
use super::*;
use crate::{CacheConfig, Ext2Error, Owner};

const BYTES: u64 = 4 << 20;

/// How many seeds the heavier tests below run (`for_seeds` may ask for more).
const CASES: usize = 48;

fn cached(io: &MemIo, blocks: usize) -> Ext2 {
    open_cached(io, blocks)
}

/// Direct I/O, but with frees deferred to commits exactly as a cached volume
/// does: the reference a cached volume must match byte for byte.
fn direct_deferred(io: &MemIo) -> Ext2 {
    open(io).with_deferred_frees()
}

#[test]
fn cached_images_are_byte_identical_to_direct_ones() {
    let mut case = 0;
    for_seeds(
        "cached_images_are_byte_identical_to_direct_ones",
        |_, rng| {
            case += 1;
            if case > workload::cases(CASES) {
                return;
            }
            let block_size = BLOCK_SIZES[rng.below(3) as usize];
            let ios: Vec<MemIo> = (0..3).map(|_| formatted(BYTES, block_size)).collect();
            // A tiny cache (constant eviction and pressure writebacks), a roomy
            // one, and the direct reference.
            let volumes = [
                cached(&ios[0], 6),
                cached(&ios[1], 4096),
                direct_deferred(&ios[2]),
            ];
            let refs: Vec<&Ext2> = volumes.iter().collect();
            let mut model = Model::default();
            for _ in 0..rng.range(20, 90) {
                if !workload::step(&refs, &mut model, rng) {
                    break;
                }
                if rng.one_in(3) {
                    // Read-your-writes, straight from the caches.
                    workload::verify(&volumes[0], &model);
                    workload::verify(&volumes[1], &model);
                }
                if rng.one_in(8) {
                    same_after_flush(&volumes, &ios);
                }
            }
            same_after_flush(&volumes, &ios);
            drop(volumes);
            workload::verify(&open(&ios[0]), &model);
        },
    );
}

/// Flush every volume; their images must be identical and clean.
fn same_after_flush(volumes: &[Ext2], ios: &[MemIo]) {
    for volume in volumes {
        volume.flush().expect("flush");
    }
    let reference = ios[ios.len() - 1].snapshot();
    for io in ios {
        assert!(
            io.snapshot() == reference,
            "a cached image differs from the direct one"
        );
    }
    assert_clean(&ios[0]);
}

/// Without deferral the cache changes nothing at all: the same operations on
/// a plain direct volume give the same image when nothing is freed.
#[test]
fn a_cache_without_frees_matches_the_plain_driver() {
    let direct_io = formatted(BYTES, 1024);
    let cached_io = formatted(BYTES, 1024);
    let direct = open(&direct_io);
    let cache = cached(&cached_io, 32);
    for fs in [&direct, &cache] {
        fs.mkdir("/d", 0o755, Owner::ROOT).unwrap();
        for file in 0..40 {
            let data = std::vec![file as u8; 700 * file];
            fs.write_file(&std::format!("/d/f{file}"), &data, 0o644, 0, 0, 1)
                .unwrap();
        }
        fs.flush().unwrap();
    }
    assert!(direct_io.snapshot() == cached_io.snapshot());
}

#[test]
fn a_failed_writeback_is_reported_and_retried() {
    let io = formatted(BYTES, 4096);
    let fs = cached(&io, 256);
    fs.mkdir("/d", 0o755, Owner::ROOT).unwrap();
    fs.write_file("/d/f", &[7u8; 50_000], 0o644, 0, 0, 1)
        .unwrap();
    io.fail_write_number(0); // the first writeback request fails
    assert_eq!(fs.flush(), Err(Ext2Error::Io));
    // Nothing was dropped: the retry lands everything, and the error has
    // been reported once already.
    assert_eq!(fs.flush(), Ok(()));
    drop(fs);
    assert_clean(&io);
    let again = open(&io);
    assert!(
        again.had_errors_at_mount(),
        "the error bit reached the disk"
    );
    let mut back = std::vec![0u8; 50_000];
    assert_eq!(again.read("/d/f", 0, &mut back), Ok(50_000));
    assert!(back.iter().all(|&byte| byte == 7));
}

#[test]
fn a_dead_disk_fails_writers_instead_of_dropping_their_data() {
    let io = formatted(BYTES, 4096);
    let fs = cached(&io, 64); // 32 dirty blocks at most
    fs.mkdir("/d", 0o755, Owner::ROOT).unwrap();
    fs.flush().unwrap();
    // Dirty the volume first, so what fails below is the writeback alone.
    fs.write_file("/d/warm", &[1u8; 100], 0o644, 0, 0, 1)
        .unwrap();
    io.fail_writes_after(0);
    fs.create("/d/f", 0o644, Owner::ROOT).unwrap();
    let chunk = [1u8; 8192];
    let mut landed = 0u64;
    loop {
        // A write that fails part-way reports what landed; the next one
        // meets the error with nothing written.
        match fs.write("/d/f", landed, &chunk) {
            Ok(8192) => landed += 8192,
            Ok(short) => {
                landed += short as u64;
                assert_eq!(fs.write("/d/f", landed, &chunk), Err(Ext2Error::Io));
                break;
            }
            Err(error) => {
                assert_eq!(error, Ext2Error::Io, "the dirty limit forces a writeback");
                break;
            }
        }
        assert!(landed < 1 << 20, "a dead disk must stop the writer");
        // Until the cache is full of dirty blocks, reads see the writes.
        let mut back = std::vec![0u8; landed as usize];
        assert_eq!(fs.read("/d/f", 0, &mut back), Ok(landed as usize));
    }
    assert_eq!(fs.flush(), Err(Ext2Error::Io));
    assert_eq!(fs.writeback(), Err(Ext2Error::Io));
}

#[test]
fn writeback_makes_data_durable_but_leaves_the_volume_dirty() {
    let io = formatted(BYTES, 1024);
    let fs = cached(&io, 512);
    fs.write_file("/f", &[3u8; 20_000], 0o644, 0, 0, 1).unwrap();
    assert!(fs.dirty_blocks() > 0);
    fs.writeback().unwrap();
    assert_eq!(fs.dirty_blocks(), 0);
    // A crash now: the bytes on disk have the file, flagged not clean.
    let crashed = MemIo::from_bytes(io.snapshot());
    let after = open(&crashed);
    assert!(!after.was_clean_at_mount());
    let mut back = std::vec![0u8; 20_000];
    assert_eq!(after.read("/f", 0, &mut back), Ok(20_000));
    assert_clean(&crashed);
}

#[test]
fn frees_wait_for_the_commit_but_count_as_free() {
    let io = formatted(BYTES, 1024);
    let fs = cached(&io, 512);
    let empty = fs.free_blocks().unwrap();
    fs.write_file("/f", &[1u8; 100_000], 0o644, 0, 0, 1)
        .unwrap();
    fs.flush().unwrap();
    fs.unlink("/f").unwrap();
    assert_eq!(
        fs.free_blocks().unwrap(),
        empty,
        "statfs sees the space at once"
    );
    assert_eq!(fs.statfs().unwrap().blocks_free, u64::from(empty));
    fs.flush().unwrap();
    assert_eq!(fs.free_blocks().unwrap(), empty);
    drop(fs);
    assert_clean(&io);
}

#[test]
fn a_full_volume_commits_its_pending_frees_and_retries() {
    let io = formatted(1 << 20, 1024);
    let fs = cached(&io, 64);
    let chunk = std::vec![9u8; 64 * 1024];
    let mut written = 0;
    while fs
        .write_file(&std::format!("/f{written}"), &chunk, 0o644, 0, 0, 1)
        .is_ok()
    {
        written += 1;
    }
    assert!(written > 4);
    for file in 0..written {
        let _ = fs.unlink(&std::format!("/f{file}"));
    }
    let _ = fs.unlink(&std::format!("/f{written}")); // the partial one
                                                     // No flush: only the deferred frees make room for these.
    for file in 0..written - 1 {
        fs.write_file(&std::format!("/g{file}"), &chunk, 0o644, 0, 0, 1)
            .unwrap_or_else(|e| panic!("g{file}: {e:?}"));
    }
    fs.flush().unwrap();
    drop(fs);
    assert_clean(&io);
}

#[test]
fn a_shrunk_cache_still_serves_the_right_bytes() {
    let io = formatted(BYTES, 2048);
    let fs = cached(&io, 128);
    let mut rng = Rng::new(7);
    let data = rng.bytes(150_000);
    fs.write_file("/f", &data, 0o644, 0, 0, 1).unwrap();
    fs.flush().unwrap();
    assert!(fs.shrink_cache() > 0);
    assert_eq!(fs.cache_stats().unwrap().pages, 0);
    let mut back = std::vec![0u8; data.len()];
    assert_eq!(fs.read("/f", 0, &mut back), Ok(data.len()));
    assert!(back == data);
    let stats = fs.cache_stats().unwrap();
    assert!(
        stats.readahead > 0,
        "a sequential read reads ahead: {stats:?}"
    );
}

#[test]
fn sequential_reads_take_few_requests() {
    let io = formatted(BYTES, 4096);
    let fs = cached(&io, 1024);
    fs.write_file("/f", &std::vec![5u8; 1 << 20], 0o644, 0, 0, 1)
        .unwrap();
    fs.flush().unwrap();
    drop(fs);
    let fs = cached(&io, 1024);
    let before = io.counters();
    let mut back = std::vec![0u8; 1 << 20];
    assert_eq!(fs.read("/f", 0, &mut back), Ok(1 << 20));
    let reads = io.counters().reads - before.reads;
    // 256 data blocks at 16 per request, plus the metadata on the way.
    assert!(reads < 40, "{reads} read requests for 1 MiB");
}

#[test]
fn an_unconfigured_cache_is_the_direct_driver() {
    let io = formatted(BYTES, 1024);
    let fs = Ext2::open_cached(Box::new(io.clone()), clock, CacheConfig::heap(0)).unwrap();
    assert!(fs.cache_stats().is_none());
}
