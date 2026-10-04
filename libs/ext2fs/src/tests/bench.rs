//! I/O cost of populating a tree: how many device requests a 30 MB package
//! tree takes. `bench_30mb_tree` prints the numbers (run it with
//! `cargo test -p ext2fs bench -- --ignored --nocapture`); the small variant
//! below runs every time and keeps the counts from regressing.

use super::*;
use crate::memio::Counters;

/// Files of a package-like tree: many small files, some medium, a few large.
fn tree(total: usize) -> Vec<(String, usize)> {
    let mut files = Vec::new();
    let mut used = 0usize;
    let mut index = 0usize;
    while used < total {
        let size = match index % 10 {
            0..=5 => 3_000 + (index * 977) % 9_000,
            6..=8 => 40_000 + (index * 7_919) % 120_000,
            _ => 600_000 + (index * 104_729) % 900_000,
        };
        files.push((std::format!("/apps/p{:02}/f{index:04}", index % 40), size));
        used += size;
        index += 1;
    }
    files
}

/// Populate `total` bytes of tree on a fresh volume opened by `open`, flush,
/// and return the device counters for the populate + flush alone.
pub fn populate(total: usize, open: impl Fn(&MemIo) -> Ext2) -> (Counters, u32) {
    let io = formatted(128 << 20, 4096);
    let fs = open(&io);
    let free_before = fs.free_blocks().unwrap();
    let before = io.counters();
    for (path, size) in tree(total) {
        let dir = &path[..path.rfind('/').unwrap()];
        fs.mkdir_p(dir, 0o755, 0, 0).unwrap();
        let data: Vec<u8> = (0..size).map(|i| (i * 31 + size) as u8).collect();
        fs.write_file(&path, &data, 0o644, 0, 0, 1).unwrap();
    }
    fs.flush().unwrap();
    let after = io.counters();
    let allocated = free_before - fs.free_blocks().unwrap();
    drop(fs);
    assert_clean(&io);
    let delta = Counters {
        reads: after.reads - before.reads,
        read_bytes: after.read_bytes - before.read_bytes,
        writes: after.writes - before.writes,
        write_bytes: after.write_bytes - before.write_bytes,
        flushes: after.flushes - before.flushes,
    };
    (delta, allocated)
}

fn print(label: &str, (c, allocated): (Counters, u32)) {
    std::println!(
        "{label}: {allocated} blocks allocated; {} reads ({} KiB), {} writes ({} KiB), \
         {} flushes; {:.2} write requests per allocated block",
        c.reads,
        c.read_bytes / 1024,
        c.writes,
        c.write_bytes / 1024,
        c.flushes,
        c.writes as f64 / f64::from(allocated.max(1)),
    );
}

#[test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_30mb_tree() {
    print("uncached", populate(30 << 20, open));
    // The kernel's default on a 256 MiB guest: 8 MiB of cache.
    print(
        "cached (8 MiB)",
        populate(30 << 20, |io| open_cached(io, 2048)),
    );
    print(
        "cached (1 MiB)",
        populate(30 << 20, |io| open_cached(io, 256)),
    );
}

/// The cache must cut write requests per allocated block several times over;
/// the small tree keeps this checked on every run.
#[test]
fn bench_small_tree_counts() {
    let (direct, allocated) = populate(4 << 20, open);
    let (cached, cached_allocated) = populate(4 << 20, |io| open_cached(io, 2048));
    assert_eq!(allocated, cached_allocated);
    assert!(allocated > 1000, "allocated {allocated}");
    assert!(
        cached.writes * 8 < direct.writes,
        "cached {} writes vs direct {}",
        cached.writes,
        direct.writes
    );
    assert!(cached.reads * 8 < direct.reads, "{cached:?} vs {direct:?}");
}

/// CPU cost of streaming one large file through the cache: what the kernel's
/// sequential write and read paths spend outside the device. Prints MB/s.
#[test]
#[ignore = "benchmark: run with --release --ignored --nocapture"]
fn bench_stream_64mb() {
    let io = formatted(512 << 20, 4096);
    let fs = open_cached(&io, 8192);
    fs.create("/big", 0o644, crate::Owner { uid: 0, gid: 0 })
        .unwrap();
    let chunk = std::vec![0x5au8; 1 << 20];
    let start = std::time::Instant::now();
    for index in 0..64u64 {
        let mut done = 0;
        while done < chunk.len() {
            done += fs
                .write(
                    "/big",
                    index * (1 << 20) + done as u64,
                    &chunk[done..(done + 65536).min(chunk.len())],
                )
                .unwrap();
        }
    }
    let wrote = start.elapsed();
    fs.flush().unwrap();
    let flushed = start.elapsed();
    let mut buf = std::vec![0u8; 1 << 20];
    let start = std::time::Instant::now();
    for index in 0..64u64 {
        let mut done = 0;
        while done < buf.len() {
            let end = (done + 65536).min(buf.len());
            done += fs
                .read("/big", index * (1 << 20) + done as u64, &mut buf[done..end])
                .unwrap();
        }
    }
    let read = start.elapsed();
    std::println!(
        "write {:.1} MB/s (flush {} ms), read {:.1} MB/s; {:?}",
        64.0 / wrote.as_secs_f64(),
        (flushed - wrote).as_millis(),
        64.0 / read.as_secs_f64(),
        fs.cache_stats()
    );
}
