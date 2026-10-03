//! The ext2 write-back block cache in the kernel (`libs/ext2fs/src/cache`,
//! `fs/ext2/cache.rs`, `fs/flusher.rs`): frames as cache pages, the VFS
//! durability points (`fsync`, `sync`/power-off, the periodic writeback,
//! memory pressure), write errors from a fault-injecting disk, and the
//! virtio-blk scatter/gather requests the writeback is built from.
//!
//! The library's own host tests prove the crash ordering and that a cached
//! image is byte-identical to a direct one; these prove the kernel wiring.

use super::*;
use crate::block::BlockDevice;
use crate::mem;

mod confd_cut;
mod soak;
mod virtio;

pub(in crate::tests) const CASES: &[(&str, Test)] = &[
    (
        "bcache_read_your_writes_and_fsync",
        read_your_writes_and_fsync,
    ),
    ("bcache_write_error_reaches_sync", write_error_reaches_sync),
    (
        "bcache_dead_disk_fails_the_writer",
        dead_disk_fails_the_writer,
    ),
    ("bcache_tiny_cache_evicts", tiny_cache_evicts),
    ("bcache_periodic_writeback", periodic_writeback),
    ("bcache_power_off_sync", power_off_sync),
    ("bcache_pressure_returns_frames", pressure_returns_frames),
    (
        "bcache_confd_power_cut_sweep",
        confd_cut::confd_power_cut_sweep,
    ),
    ("bcache_virtio_scatter_gather", virtio::scatter_gather),
    ("bcache_virtio_cached_volume", virtio::cached_volume),
    ("bcache_soak_block_ops", soak::block_ops),
    ("bcache_soak_remount_cycles", soak::remount_cycles),
];

/// Volume size in 1 KiB blocks (one group): 1 MiB of kernel heap per disk,
/// which a heap fragmented by the earlier suites can still find in one piece.
const BLOCKS: u32 = 1024;

/// The suite's disks, leaked once and refilled per test; [`release`] hands
/// their memory back to the heap.
fn pooled(slot: usize) -> &'static FakeDisk {
    const NAMES: [&str; 2] = ["bcache-a", "bcache-b"];
    static DISKS: spin::Mutex<[Option<&'static FakeDisk>; 2]> = spin::Mutex::new([None; 2]);
    let disk = *DISKS.lock()[slot].get_or_insert_with(|| FakeDisk::new(NAMES[slot], 0));
    disk.fail_nth_write(u32::MAX);
    disk.set_read_only(false);
    disk
}

fn release(disk: &FakeDisk) {
    *disk.data.lock() = Vec::new();
}

/// A fresh 1 KiB-block volume on `slot`, formatted by `libs/ext2fs`.
fn formatted(slot: usize) -> Result<&'static FakeDisk, String> {
    let disk = pooled(slot);
    *disk.data.lock() = vec![0u8; BLOCKS as usize * 1024];
    let geometry = ext2fs::Geometry {
        block_size: 1024,
        blocks_count: BLOCKS,
        bytes_per_inode: 4096,
    };
    let device: &'static dyn BlockDevice = disk;
    ext2fs::format(&device, geometry, "bcache", [0x42; 16], 1_700_000_000)
        .map_err(|error| format!("format: {error:?}"))?;
    Ok(disk)
}

/// `disk` mounted through a cache of `pages` frames, as a private VFS root.
fn cached(disk: &'static FakeDisk, pages: usize) -> Result<(Arc<Ext2>, Vfs), String> {
    let fs = Arc::new(Ext2::open_with_pages(disk, pages).map_err(fs_error)?);
    let mut vfs = Vfs::new();
    vfs.mount("/", fs.clone(), crate::fs::vfs::MountFlags::default())
        .map_err(fs_error)?;
    Ok((fs, vfs))
}

/// A copy of `from`'s bytes on the other disk, mounted directly: what a
/// reboot right now would find, without disturbing the live volume.
fn reboot_copy(from: &FakeDisk) -> Result<(&'static FakeDisk, Arc<Ext2>, Vfs), String> {
    let copy = pooled(1);
    {
        // One allocation: no temporary copy on a tight heap.
        let source = from.data.lock();
        let mut target = copy.data.lock();
        target.clear();
        target.extend_from_slice(&source);
    }
    let (fs, vfs) = remount_disk(copy)?;
    Ok((copy, fs, vfs))
}

fn write_file(vfs: &mut Vfs, path: &str, data: &[u8]) -> Result<(), String> {
    match vfs.create(Id::ROOT, path, 0o644) {
        Ok(_) | Err(FsError::Exists) => {}
        Err(error) => return Err(fs_error(error)),
    }
    let written = vfs.write(Id::ROOT, path, 0, data).map_err(fs_error)?;
    check!(written == data.len(), "{path}: short write");
    Ok(())
}

fn read_file(vfs: &mut Vfs, path: &str) -> Result<Vec<u8>, String> {
    vfs.read_file(Id::ROOT, path).map_err(fs_error)
}

/// Writes are served from the cache before anything reaches the disk, and
/// `fsync` (`Vfs::flush`) makes them durable and the volume clean.
pub fn read_your_writes_and_fsync() -> Result<(), String> {
    task::register_kernel();
    let disk = formatted(0)?;
    let (fs, mut vfs) = cached(disk, 256)?;
    let files: Vec<(String, Vec<u8>)> = (0..12u32)
        .map(|n| (format!("/f{n}"), pattern_bytes(n, 1 + n as usize * 9_000)))
        .collect();
    for (path, data) in &files {
        write_file(&mut vfs, path, data)?;
        check!(
            read_file(&mut vfs, path)? == *data,
            "{path}: read-your-writes"
        );
    }
    check!(fs.dirty_blocks() > 50, "the writes are still in the cache");
    check!(raw_state(disk) & 1 == 0, "the volume is marked dirty first");
    vfs.flush(Id::ROOT, "/f3").map_err(fs_error)?;
    check!(fs.dirty_blocks() == 0, "fsync wrote everything back");
    check!(raw_state(disk) & 1 == 1, "fsync marked the volume clean");
    check_volume(disk, BLOCKS)?;
    let (_, _, mut after) = reboot_copy(disk)?;
    for (path, data) in &files {
        check!(
            read_file(&mut after, path)? == *data,
            "{path} after a reboot"
        );
    }
    drop((fs, vfs, after));
    release(disk);
    release(pooled(1));
    Ok(())
}

/// A writeback the disk refuses is reported by the next sync, retried, and
/// recorded in `s_state`; nothing written is lost.
pub fn write_error_reaches_sync() -> Result<(), String> {
    task::register_kernel();
    let disk = formatted(0)?;
    let (fs, mut vfs) = cached(disk, 256)?;
    let data = pattern_bytes(7, 40_000);
    write_file(&mut vfs, "/f", &data)?;
    disk.fail_nth_write(1);
    check!(fs.flush().is_err(), "the failed writeback was not reported");
    check!(fs.flush().is_ok(), "the retry did not land");
    drop((fs, vfs));
    check!(
        raw_state(disk) & 2 == 2,
        "s_state does not carry the error bit"
    );
    let (fs, mut vfs) = remount_disk(disk)?;
    check!(
        fs.had_errors_at_mount(),
        "the next mount does not see the error"
    );
    check!(read_file(&mut vfs, "/f")? == data, "data was lost");
    check_volume(disk, BLOCKS)?;
    drop((fs, vfs));
    release(disk);
    Ok(())
}

/// With the disk gone, the writer meets the error once the dirty limit
/// forces a writeback, and every later sync fails: nothing is dropped
/// silently. Once the disk is back, the data still lands.
pub fn dead_disk_fails_the_writer() -> Result<(), String> {
    task::register_kernel();
    let disk = formatted(0)?;
    let (fs, mut vfs) = cached(disk, 64)?;
    write_file(&mut vfs, "/warm", b"dirty first")?;
    disk.cut_power_at(1);
    vfs.create(Id::ROOT, "/f", 0o644).map_err(fs_error)?;
    let chunk = pattern_bytes(3, 4096);
    let mut landed = 0u64;
    let mut failed = false;
    for _ in 0..200 {
        match vfs.write(Id::ROOT, "/f", landed, &chunk) {
            Ok(count) if count == chunk.len() => landed += count as u64,
            Ok(count) => landed += count as u64,
            Err(_) => {
                failed = true;
                break;
            }
        }
    }
    check!(failed, "a writer never learned that the disk is dead");
    check!(fs.flush().is_err(), "sync claimed success on a dead disk");
    check!(fs.flush().is_err(), "the second sync too");
    disk.fail_nth_write(u32::MAX); // the disk comes back
    check!(fs.flush().is_ok(), "the retained blocks did not land");
    drop((fs, vfs));
    let (fs, mut vfs) = remount_disk(disk)?;
    let back = read_file(&mut vfs, "/f")?;
    check!(
        back.len() as u64 == landed,
        "{} bytes back, {landed} landed",
        back.len()
    );
    check!(
        back.chunks(4096).all(|part| part == &chunk[..part.len()]),
        "data damaged"
    );
    check_volume(disk, BLOCKS)?;
    drop((fs, vfs));
    release(disk);
    Ok(())
}

/// A four-page cache still serves every byte right: it evicts clean blocks
/// and writes dirty ones back to make room.
pub fn tiny_cache_evicts() -> Result<(), String> {
    task::register_kernel();
    let disk = formatted(0)?;
    let (fs, mut vfs) = cached(disk, 4)?;
    let mut rng = Rng(0x2545_F491);
    let data = pattern_bytes(11, 300_000);
    vfs.create(Id::ROOT, "/big", 0o644).map_err(fs_error)?;
    let mut at = 0;
    while at < data.len() {
        let len = (1 + rng.below(20_000) as usize).min(data.len() - at);
        vfs.write(Id::ROOT, "/big", at as u64, &data[at..at + len])
            .map_err(fs_error)?;
        at += len;
    }
    for _ in 0..200 {
        let start = rng.below(data.len() as u32) as usize;
        let len = (1 + rng.below(5_000) as usize).min(data.len() - start);
        let mut buf = vec![0u8; len];
        vfs.read(Id::ROOT, "/big", start as u64, &mut buf)
            .map_err(fs_error)?;
        check!(
            buf == data[start..start + len],
            "bytes {start}+{len} differ"
        );
    }
    let stats = fs.cache_stats().ok_or("no cache")?;
    check!(stats.pages <= 4 && stats.evictions > 100, "{stats:?}");
    fs.flush().map_err(fs_error)?;
    check_volume(disk, BLOCKS)?;
    drop((fs, vfs));
    let (fs, mut vfs) = remount_disk(disk)?;
    check!(
        read_file(&mut vfs, "/big")? == data,
        "the file after a remount"
    );
    drop((fs, vfs));
    release(disk);
    Ok(())
}

/// The periodic writeback puts the data on the disk but leaves the volume
/// flagged dirty; the kernel's flusher runs it without blocking.
pub fn periodic_writeback() -> Result<(), String> {
    task::register_kernel();
    let disk = formatted(0)?;
    let (fs, mut vfs) = cached(disk, 256)?;
    let data = pattern_bytes(5, 70_000);
    write_file(&mut vfs, "/f", &data)?;
    vfs.unlink(Id::ROOT, "/f").map_err(fs_error)?;
    write_file(&mut vfs, "/g", &data)?;
    vfs.writeback_all(false).map_err(fs_error)?;
    check!(fs.dirty_blocks() == 0, "the writeback left dirty blocks");
    check!(
        raw_state(disk) & 1 == 0,
        "a writeback must not mark the volume clean"
    );
    let (copy, after_fs, mut after) = reboot_copy(disk)?;
    check!(
        read_file(&mut after, "/g")? == data,
        "the written-back file"
    );
    check!(
        read_file(&mut after, "/f").is_err(),
        "the unlinked file came back"
    );
    // The writeback committed the deferred frees: the counters agree.
    check_volume(copy, BLOCKS)?;
    drop((after_fs, after));
    crate::fs::flusher::service(); // the global flusher: never blocks or panics
    drop((fs, vfs));
    release(disk);
    release(copy);
    Ok(())
}

/// What the power path runs (`fs::sync_all` -> `Vfs::sync_all`): every block
/// written back, then the clean marker last.
pub fn power_off_sync() -> Result<(), String> {
    task::register_kernel();
    let disk = formatted(0)?;
    let (fs, mut vfs) = cached(disk, 256)?;
    vfs.mkdir(Id::ROOT, "/apps", 0o755).map_err(fs_error)?;
    let data = pattern_bytes(9, 120_000);
    write_file(&mut vfs, "/apps/blob", &data)?;
    check!(raw_state(disk) & 1 == 0, "dirty while running");
    vfs.sync_all().map_err(fs_error)?;
    check!(fs.dirty_blocks() == 0, "sync left dirty blocks");
    check!(
        raw_state(disk) & 1 == 1,
        "sync did not mark the volume clean"
    );
    check_volume(disk, BLOCKS)?;
    // The machine stops here: nothing after the sync may be needed.
    let (_, after_fs, mut after) = reboot_copy(disk)?;
    check!(after_fs.was_clean_at_mount(), "not clean at the next boot");
    check!(
        read_file(&mut after, "/apps/blob")? == data,
        "data after power-off"
    );
    drop((fs, vfs, after_fs, after));
    release(disk);
    release(pooled(1));
    Ok(())
}

/// Memory pressure gives the cache's clean frames back, and an unmount
/// returns all of them.
pub fn pressure_returns_frames() -> Result<(), String> {
    task::register_kernel();
    let disk = formatted(0)?;
    let before = mem::frame_stats().live();
    let (fs, mut vfs) = cached(disk, 512)?;
    let data = pattern_bytes(2, 200_000);
    write_file(&mut vfs, "/f", &data)?;
    let held = mem::frame_stats().live() - before;
    check!(held > 150, "the cache holds only {held} frames");
    vfs.writeback_all(true).map_err(fs_error)?;
    let stats = fs.cache_stats().ok_or("no cache")?;
    check!(stats.pages == 0, "pressure kept {} pages", stats.pages);
    check!(read_file(&mut vfs, "/f")? == data, "reads after a shrink");
    drop((fs, vfs));
    let after = mem::frame_stats().live();
    check!(
        after == before,
        "frames leaked: {before} before, {after} after"
    );
    release(disk);
    Ok(())
}
