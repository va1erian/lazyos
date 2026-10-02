//! The cache over the real driver: virtio-blk scatter/gather requests, and a
//! cached ext2 volume on the scratch disk `tools/test/run.py` attaches. The
//! tests use the scratch disk's upper half only (the `virtio_suite` window and
//! its untouched neighbours are below) and skip without a scratch disk.

use super::*;
use crate::block::partition::Partition;
use crate::block::SECTOR_SIZE;
use alloc::boxed::Box;

/// Sectors of the scratch disk the runner attaches (16 MiB).
const SCRATCH_SECTORS: u64 = 32 * 1024;
/// Where this suite's half of the scratch disk starts.
const HALF: u64 = 16 * 1024 + 64;

fn scratch(test: &str) -> Option<&'static dyn BlockDevice> {
    let boot = crate::block::boot_device().map(|device| device.name());
    let found = crate::block::devices().into_iter().find(|device| {
        device.name().starts_with("virtio")
            && Some(device.name()) != boot
            && device.sector_count() == SCRATCH_SECTORS
    });
    if found.is_none() {
        serial_println!("TEST:{test}:INFO:no scratch virtio disk; skipped");
    }
    found
}

/// A byte for every absolute offset, so no reference buffer is needed.
fn byte_at(offset: usize, salt: u8) -> u8 {
    (offset.wrapping_mul(0x9E37_79B1) >> 13) as u8 ^ salt
}

/// Segment lengths (in sectors) that straddle the 64 KiB request boundary in
/// every way: 4 KiB pages, odd runs, and one segment longer than a request.
const SEGMENTS: [usize; 9] = [8, 3, 8, 8, 1, 130, 8, 5, 8];

/// Vectored writes and reads move one byte range, whatever the segmentation,
/// in as few requests as the bounce region allows.
pub fn scatter_gather() -> Result<(), String> {
    let Some(disk) = scratch("bcache_virtio_scatter_gather") else {
        return Ok(());
    };
    let total: usize = SEGMENTS.iter().sum::<usize>() * SECTOR_SIZE;
    for salt in [0x5Au8, 0xA5] {
        let source: Vec<u8> = (0..total).map(|at| byte_at(at, salt)).collect();
        let mut parts: Vec<&[u8]> = Vec::new();
        let mut rest = &source[..];
        for sectors in SEGMENTS {
            let (part, tail) = rest.split_at(sectors * SECTOR_SIZE);
            parts.push(part);
            rest = tail;
        }
        let before = disk.stats().map(|stats| stats.snapshot());
        disk.write_sectors_vectored(HALF, &parts)
            .map_err(|e| format!("vectored write: {e:?}"))?;
        if let (Some(before), Some(stats)) = (before, disk.stats()) {
            let requests = stats.snapshot().writes - before.writes;
            check!(
                requests == total.div_ceil(64 * 1024) as u64,
                "{total} bytes took {requests} write requests"
            );
        }
        // Read back with another segmentation, and as one buffer.
        let mut back = vec![0u8; total];
        {
            let (first, second) = back.split_at_mut(3 * SECTOR_SIZE + 4096);
            let (second, third) = second.split_at_mut(70 * 1024);
            disk.read_sectors_vectored(HALF, &mut [first, second, third])
                .map_err(|e| format!("vectored read: {e:?}"))?;
        }
        check!(
            back == source,
            "vectored read-back differs (salt {salt:#x})"
        );
        back.fill(0);
        disk.read_sectors(HALF, &mut back)
            .map_err(|e| format!("plain read: {e:?}"))?;
        check!(back == source, "plain read-back differs (salt {salt:#x})");
    }
    Ok(())
}

/// The upper half of the scratch disk as its own device, leaked once.
fn window(disk: &'static dyn BlockDevice) -> &'static Partition {
    static WINDOW: spin::Mutex<Option<&'static Partition>> = spin::Mutex::new(None);
    *WINDOW.lock().get_or_insert_with(|| {
        let sectors = SCRATCH_SECTORS - HALF - 64;
        Box::leak(Box::new(Partition::new(
            disk,
            "bcache-scratch",
            HALF + 64,
            sectors,
        )))
    })
}

/// A cached 4 KiB-block volume on real virtio-blk: a tree of files written,
/// synced, and read back after a remount, in far fewer requests than blocks.
pub fn cached_volume() -> Result<(), String> {
    task::register_kernel();
    let Some(disk) = scratch("bcache_virtio_cached_volume") else {
        return Ok(());
    };
    let device: &'static dyn BlockDevice = window(disk);
    let geometry = ext2fs::Geometry::for_size(device.sector_count() * SECTOR_SIZE as u64);
    ext2fs::format(
        &device,
        geometry,
        "bcache-virtio",
        [0x77; 16],
        1_700_000_000,
    )
    .map_err(|error| format!("format: {error:?}"))?;
    let before = disk
        .stats()
        .map(|stats| stats.snapshot())
        .unwrap_or_default();
    let fs = Arc::new(Ext2::open_cached(device).map_err(fs_error)?);
    let mut vfs = Vfs::new();
    vfs.mount("/", fs.clone(), crate::fs::vfs::MountFlags::default())
        .map_err(fs_error)?;
    let files: Vec<(String, Vec<u8>)> = (0..24u32)
        .map(|n| {
            (
                format!("/pkg/f{n}"),
                pattern_bytes(n, 2_000 + n as usize * 11_000),
            )
        })
        .collect();
    vfs.mkdir(Id::ROOT, "/pkg", 0o755).map_err(fs_error)?;
    for (path, data) in &files {
        write_file(&mut vfs, path, data)?;
    }
    vfs.sync_all().map_err(fs_error)?;
    let after = disk
        .stats()
        .map(|stats| stats.snapshot())
        .unwrap_or_default();
    let bytes: usize = files.iter().map(|(_, data)| data.len()).sum();
    let blocks = bytes.div_ceil(4096) as u64;
    let writes = after.writes - before.writes;
    serial_println!(
        "TEST:bcache_virtio_cached_volume:INFO:{blocks} data blocks in {writes} write requests"
    );
    check!(
        writes * 4 < blocks,
        "{writes} write requests for {blocks} blocks"
    );
    drop((fs, vfs));
    let fs = Arc::new(Ext2::open(device).map_err(fs_error)?);
    check!(fs.was_clean_at_mount(), "the synced volume is not clean");
    let mut vfs = Vfs::new();
    vfs.mount("/", fs.clone(), crate::fs::vfs::MountFlags::default())
        .map_err(fs_error)?;
    for (path, data) in &files {
        check!(
            read_file(&mut vfs, path)? == *data,
            "{path} after a remount"
        );
    }
    Ok(())
}
