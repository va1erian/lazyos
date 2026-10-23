//! Timings of the image stages on a file set shaped like a LazyWeb desktop
//! image (a few 100 MiB programs, a few dozen multi-MiB apps, hundreds of
//! small documents). Ignored by default; run it with
//!
//! ```text
//! cargo test -p build-support-tests --release bench_ -- --ignored --nocapture
//! ```
//!
//! to see what a rebuild spends writing the OS volume (`os_image::compose`)
//! and the stick's RAM root (`usb_ramdisk::write`).

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::layout_tests::fake_mbr;
use crate::os_image::{compose, plan, OsFile, Placement, Settings, Source};
use crate::os_layout::{dirs, parse_passwd};
use crate::usb_ramdisk::{self, Settings as RamSettings};

const MIB: u64 = 1 << 20;

/// Deterministic, incompressible-looking bytes (xorshift), so the volume
/// writes real blocks the way a program's code does.
fn noise(len: u64, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len as usize);
    while (out.len() as u64) < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len as usize);
    out
}

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/bench-tmp")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The file set: `(path, size)`, about 380 MiB like the real image.
fn shape() -> Vec<(String, u64)> {
    let mut files = Vec::new();
    for name in ["webprocess", "networkprocess"] {
        files.push((format!("/system/bin/{name}"), 107 * MIB));
    }
    files.push(("/system/share/samples/lazyweb.lzp".into(), 43 * MIB));
    for index in 0..60 {
        files.push((format!("/system/bin/prog{index}"), 3 * MIB));
    }
    for index in 0..15 {
        files.push((format!("/system/share/samples/app{index}.lzp"), 3 * MIB / 2));
    }
    for index in 0..400 {
        files.push((format!("/docs/os/doc{index}.md"), 8_000 + index * 40));
    }
    for index in 0..60 {
        files.push((format!("/system/share/data/file{index}.bin"), 200_000));
    }
    files
}

fn make_files(dir: &Path) -> Vec<OsFile> {
    shape()
        .into_iter()
        .enumerate()
        .map(|(index, (path, size))| {
            let host = dir.join(format!("src{index}"));
            std::fs::write(&host, noise(size, index as u64 + 7)).unwrap();
            OsFile {
                path,
                source: Source::Path(host),
                mode: 0o644,
                placement: Placement::ROOT,
            }
        })
        .collect()
}

fn timed<T>(what: &str, work: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let out = work();
    println!("BENCH {what}: {:.2}s", started.elapsed().as_secs_f64());
    out
}

#[test]
#[ignore]
fn bench_image_stages() {
    let dir = scratch("stages");
    let files = timed("make the host files", || make_files(&dir));
    let total: u64 = files.iter().map(|f| f.source.len().unwrap()).sum();
    println!("BENCH {} files, {} MiB", files.len(), total >> 20);
    let layout = dirs(&parse_passwd("user:1000:1000:x:/home/user:sh\n"));

    // The OS volume: create, then the warm update every rebuild does.
    let image = dir.join("lazyos.img");
    let settings = Settings {
        os_size: 1 << 30,
        reset: false,
        update_damaged: false,
        journal: None,
    };
    let mut bios = fake_mbr(20);
    bios.resize(25 * 512, 1);
    bios[..512].copy_from_slice(&fake_mbr(20));
    for round in ["create", "update (unchanged files)", "update (again)"] {
        let planned = plan(&image, &settings).unwrap();
        timed(&format!("os volume {round}"), || {
            compose(&planned, &image, &bios, &settings, &layout, &files).unwrap()
        });
    }

    // The stick's RAM root.
    let ramdisk = dir.join("ramdisk.img");
    let ram = RamSettings {
        uuid: [7; 16],
        root_free: 64 * MIB,
        image_id: None,
    };
    let written = timed("ramdisk write", || {
        usb_ramdisk::write(&ramdisk, &ram, &layout, &files).unwrap()
    });
    println!("BENCH ramdisk {} MiB", written.bytes >> 20);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A device that counts how long its calls take, to tell the file system's own
/// cost from the host disk's.
struct Counting {
    inner: crate::os_disk::FileIo,
    stats: std::sync::Arc<Stats>,
}

#[derive(Default)]
struct Stats {
    read_ns: std::sync::atomic::AtomicU64,
    write_ns: std::sync::atomic::AtomicU64,
    flush_ns: std::sync::atomic::AtomicU64,
    reads: std::sync::atomic::AtomicU64,
    writes: std::sync::atomic::AtomicU64,
    flushes: std::sync::atomic::AtomicU64,
    bytes: std::sync::atomic::AtomicU64,
}

impl ext2fs::BlockIo for Counting {
    fn sector_count(&self) -> u64 {
        self.inner.sector_count()
    }
    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), ext2fs::IoError> {
        use std::sync::atomic::Ordering::Relaxed;
        let t = Instant::now();
        let r = self.inner.read_sectors(lba, buf);
        self.stats
            .read_ns
            .fetch_add(t.elapsed().as_nanos() as u64, Relaxed);
        self.stats.reads.fetch_add(1, Relaxed);
        r
    }
    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), ext2fs::IoError> {
        use std::sync::atomic::Ordering::Relaxed;
        let t = Instant::now();
        let r = self.inner.write_sectors(lba, buf);
        self.stats
            .write_ns
            .fetch_add(t.elapsed().as_nanos() as u64, Relaxed);
        self.stats.writes.fetch_add(1, Relaxed);
        self.stats.bytes.fetch_add(buf.len() as u64, Relaxed);
        r
    }
    fn write_sectors_vectored(&self, lba: u64, bufs: &[&[u8]]) -> Result<(), ext2fs::IoError> {
        use std::sync::atomic::Ordering::Relaxed;
        let t = Instant::now();
        let r = self.inner.write_sectors_vectored(lba, bufs);
        self.stats
            .write_ns
            .fetch_add(t.elapsed().as_nanos() as u64, Relaxed);
        self.stats.writes.fetch_add(1, Relaxed);
        self.stats
            .bytes
            .fetch_add(bufs.iter().map(|b| b.len() as u64).sum(), Relaxed);
        r
    }
    fn read_sectors_vectored(
        &self,
        lba: u64,
        bufs: &mut [&mut [u8]],
    ) -> Result<(), ext2fs::IoError> {
        use std::sync::atomic::Ordering::Relaxed;
        let t = Instant::now();
        let r = self.inner.read_sectors_vectored(lba, bufs);
        self.stats
            .read_ns
            .fetch_add(t.elapsed().as_nanos() as u64, Relaxed);
        self.stats.reads.fetch_add(1, Relaxed);
        r
    }
    fn flush(&self) -> Result<(), ext2fs::IoError> {
        use std::sync::atomic::Ordering::Relaxed;
        let t = Instant::now();
        let r = self.inner.flush();
        self.stats
            .flush_ns
            .fetch_add(t.elapsed().as_nanos() as u64, Relaxed);
        self.stats.flushes.fetch_add(1, Relaxed);
        r
    }
    fn is_writable(&self) -> bool {
        true
    }
}

#[test]
#[ignore]
fn bench_volume_breakdown() {
    use std::sync::atomic::Ordering::Relaxed;
    let dir = scratch("breakdown");
    let files = make_files(&dir);
    let layout = dirs(&parse_passwd("user:1000:1000:x:/home/user:sh\n"));
    let path = dir.join("volume.img");
    let size = 1u64 << 30;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    crate::os_disk::mark_sparse(&file);
    file.set_len(size).unwrap();
    let stats = std::sync::Arc::new(Stats::default());
    let io = Counting {
        inner: crate::os_disk::FileIo::volatile(file, 0, size / 512, true),
        stats: stats.clone(),
    };
    let started = Instant::now();
    ext2fs::format(
        &io,
        ext2fs::Geometry::for_size(size),
        "lazyos",
        [1; 16],
        1_700_000_000,
    )
    .unwrap();
    println!("BENCH format: {:.2}s", started.elapsed().as_secs_f64());
    let mut config = ext2fs::CacheConfig::heap(16 * 1024);
    config.max_request = 1 << 20;
    let volume = ext2fs::Ext2::open_cached(Box::new(io), crate::os_image::now, config).unwrap();
    let started = Instant::now();
    crate::os_image::write_volume(&volume, None, &layout, &files, 1_700_000_000).unwrap();
    let total = started.elapsed().as_secs_f64();
    let ns = |a: &std::sync::atomic::AtomicU64| a.load(Relaxed) as f64 / 1e9;
    println!(
        "BENCH write_volume total {total:.2}s | io: read {:.2}s ({}), write {:.2}s ({}, {} MiB), flush {:.2}s ({})",
        ns(&stats.read_ns),
        stats.reads.load(Relaxed),
        ns(&stats.write_ns),
        stats.writes.load(Relaxed),
        stats.bytes.load(Relaxed) >> 20,
        ns(&stats.flush_ns),
        stats.flushes.load(Relaxed),
    );
    drop(volume);
    let _ = std::fs::remove_dir_all(&dir);
}
