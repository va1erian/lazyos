//! The boot I/O path (boot-time work): multi-sector ATA runs, FAT reads that
//! coalesce contiguous clusters, PCI enumeration, and the lazy frame cursor.
//!
//! Each optimisation here replaced a slower one-at-a-time loop, so the tests
//! pin the fast path against the simple path: whole-file reads against tiny
//! chunked reads, and runs against single-sector reads.

use super::block_suite::FakeDisk;
use super::*;
use crate::block::{self, BlockDevice, BlockError, SECTOR_SIZE};
use crate::fs::fat::Fat16;
use crate::fs::vfs::Filesystem;
use crate::mem::untouched::Untouched;

/// xorshift: deterministic pseudo-random offsets for the soak tests.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn ata() -> Result<&'static dyn BlockDevice, String> {
    block::init();
    block::device("ata0").ok_or_else(|| String::from("ata0 is not registered"))
}

/// A run of N sectors must equal N single-sector reads, at every run length
/// around the 128-sector command limit.
fn ata_runs_match_single_sector_reads() -> Result<(), String> {
    let device = ata()?;
    for (lba, count) in [
        (0u64, 1usize),
        (1, 2),
        (3, 7),
        (0, 127),
        (5, 128),
        (2, 129),
        (0, 300),
    ] {
        let mut run = vec![0u8; count * SECTOR_SIZE];
        device
            .read_sectors(lba, &mut run)
            .map_err(|error| format!("run {lba}+{count}: {error:?}"))?;
        for index in 0..count {
            let mut single = [0u8; SECTOR_SIZE];
            device
                .read_sectors(lba + index as u64, &mut single)
                .map_err(|error| format!("sector {}: {error:?}", lba + index as u64))?;
            check!(
                run[index * SECTOR_SIZE..(index + 1) * SECTOR_SIZE] == single,
                "run {lba}+{count} differs from a single read at sector {index}"
            );
        }
    }
    Ok(())
}

/// Bad lengths and ranges are refused before any port is touched.
fn ata_run_bounds() -> Result<(), String> {
    let device = ata()?;
    let mut odd = [0u8; 100];
    check!(
        device.read_sectors(0, &mut odd) == Err(BlockError::Unsupported),
        "a non-sector length was accepted"
    );
    let end = device.sector_count();
    let mut two = [0u8; 2 * SECTOR_SIZE];
    check!(
        device.read_sectors(end - 1, &mut two) == Err(BlockError::Bounds),
        "a run past the end of the disk was accepted"
    );
    let mut none: [u8; 0] = [];
    check!(
        device.read_sectors(end, &mut none).is_ok(),
        "an empty read at the end must be a no-op"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// FAT: a hand-built volume with a fragmented file
// ---------------------------------------------------------------------------

const IMAGE_SECTORS: usize = 4400;
const FAT_LBA: usize = 2;
const ROOT_LBA: usize = 20;
const DATA_LBA: usize = 52;
/// Cluster chain of the test file: runs (2) (5,6,7) (3) (9,10,11) (4).
const CHAIN: [u16; 9] = [2, 5, 6, 7, 3, 9, 10, 11, 4];
const FILE_SIZE: usize = 8 * SECTOR_SIZE + 300;

fn pattern(cluster: u16, index: usize) -> u8 {
    (cluster as u8).wrapping_mul(7).wrapping_add(index as u8) ^ (index >> 8) as u8
}

fn put16(image: &mut [u8], at: usize, value: u16) {
    image[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(image: &mut [u8], at: usize, value: u32) {
    image[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// A one-partition FAT16 image (1 sector per cluster) holding `FRAG.BIN`.
fn fragmented_image() -> Vec<u8> {
    let mut image = vec![0u8; IMAGE_SECTORS * SECTOR_SIZE];
    // MBR: one FAT16 partition starting at sector 1.
    image[0x1BE + 4] = 0x06;
    put32(&mut image, 0x1BE + 8, 1);
    put32(&mut image, 0x1BE + 12, (IMAGE_SECTORS - 1) as u32);
    image[510] = 0x55;
    image[511] = 0xAA;
    // BPB at the partition start.
    let bpb = SECTOR_SIZE;
    put16(&mut image, bpb + 11, SECTOR_SIZE as u16);
    image[bpb + 13] = 1; // sectors per cluster
    put16(&mut image, bpb + 14, 1); // reserved
    image[bpb + 16] = 1; // FATs
    put16(&mut image, bpb + 17, 512); // root entries
    put16(&mut image, bpb + 19, (IMAGE_SECTORS - 1) as u16);
    put16(&mut image, bpb + 22, 18); // sectors per FAT
                                     // The chain, in the FAT.
    for pair in CHAIN.windows(2) {
        put16(
            &mut image,
            FAT_LBA * SECTOR_SIZE + pair[0] as usize * 2,
            pair[1],
        );
    }
    put16(
        &mut image,
        FAT_LBA * SECTOR_SIZE + CHAIN[CHAIN.len() - 1] as usize * 2,
        0xFFFF,
    );
    // Root directory entry.
    let entry = ROOT_LBA * SECTOR_SIZE;
    image[entry..entry + 11].copy_from_slice(b"FRAG    BIN");
    image[entry + 11] = 0x20;
    put16(&mut image, entry + 26, CHAIN[0]);
    put32(&mut image, entry + 28, FILE_SIZE as u32);
    // Cluster contents.
    for &cluster in &CHAIN {
        let base = (DATA_LBA + cluster as usize - 2) * SECTOR_SIZE;
        for j in 0..SECTOR_SIZE {
            image[base + j] = pattern(cluster, j);
        }
    }
    image
}

fn expected_file() -> Vec<u8> {
    (0..FILE_SIZE)
        .map(|i| pattern(CHAIN[i / SECTOR_SIZE], i % SECTOR_SIZE))
        .collect()
}

/// Reads over a fragmented chain: whole file, every small window, and a soak
/// of random ranges, all against the expected bytes. The volume is opened on a
/// fake disk directly; a volume keeps its own device handle (issue #244).
fn fat_fragmented_chain_reads() -> Result<(), String> {
    let disk = FakeDisk::new("test-frag-fat", IMAGE_SECTORS);
    disk.data.lock().copy_from_slice(&fragmented_image());
    fragmented_reads(disk)
}

fn fragmented_reads(disk: &'static FakeDisk) -> Result<(), String> {
    let volume = Fat16::open(disk).ok_or_else(|| String::from("the fake FAT volume did not open"))?;
    let want = expected_file();

    let mut whole = vec![0u8; FILE_SIZE + 64];
    let got = volume
        .read("FRAG.BIN", 0, &mut whole)
        .map_err(|error| format!("whole read: {error:?}"))?;
    check!(got == FILE_SIZE, "whole read returned {got} of {FILE_SIZE}");
    check!(whole[..FILE_SIZE] == want[..], "whole read differs");

    // Every 1..=7 byte window at unaligned offsets stays in the simple path.
    for offset in (0..FILE_SIZE).step_by(97) {
        for len in [1usize, 2, 7, 511, 512, 513, 1030, 4096] {
            let mut buf = vec![0u8; len];
            let got = volume
                .read("FRAG.BIN", offset as u64, &mut buf)
                .map_err(|error| format!("read {offset}+{len}: {error:?}"))?;
            let end = (offset + len).min(FILE_SIZE);
            check!(got == end - offset, "read {offset}+{len} returned {got}");
            check!(
                buf[..got] == want[offset..end],
                "read {offset}+{len} differs"
            );
        }
    }
    check!(
        volume.read("FRAG.BIN", FILE_SIZE as u64, &mut whole) == Ok(0),
        "a read at EOF must return 0"
    );

    // Soak: random ranges, including ones that start mid-cluster and end
    // mid-run.
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for round in 0..3000 {
        let offset = (rng.next() as usize) % FILE_SIZE;
        let len = 1 + (rng.next() as usize) % (FILE_SIZE + 100);
        let mut buf = vec![0u8; len];
        let got = volume
            .read("FRAG.BIN", offset as u64, &mut buf)
            .map_err(|error| format!("soak {round}: {error:?}"))?;
        let end = (offset + len).min(FILE_SIZE);
        check!(
            got == end - offset && buf[..got] == want[offset..end],
            "soak round {round}: read {offset}+{len} differs"
        );
    }
    Ok(())
}

/// The real boot volume: a big contiguous ELF read whole must equal the same
/// file assembled from small windows (which never take the coalesced path),
/// repeatedly.
fn fat_boot_volume_whole_matches_windows() -> Result<(), String> {
    block::init();
    let boot = block::boot_device().ok_or("no boot device")?;
    let volume = Fat16::open(boot).ok_or_else(|| String::from("the boot FAT volume did not open"))?;
    let meta = volume
        .lookup("SH.ELF")
        .map_err(|error| format!("lookup: {error:?}"))?;
    let size = meta.size as usize;
    check!(
        size > 8 * SECTOR_SIZE,
        "SH.ELF is unexpectedly small ({size})"
    );
    let mut whole = vec![0u8; size];
    check!(
        volume.read("SH.ELF", 0, &mut whole) == Ok(size),
        "whole read of SH.ELF came back short"
    );
    check!(&whole[..4] == b"\x7fELF", "SH.ELF lost its ELF magic");

    let mut assembled = Vec::with_capacity(size);
    let mut window = [0u8; 300];
    let mut offset = 0usize;
    while offset < size {
        let got = volume
            .read("SH.ELF", offset as u64, &mut window)
            .map_err(|error| format!("window at {offset}: {error:?}"))?;
        check!(got > 0, "a window read stalled at {offset}");
        assembled.extend_from_slice(&window[..got]);
        offset += got;
    }
    check!(
        assembled == whole,
        "windowed and whole reads of SH.ELF differ"
    );

    // Soak: the whole file again and again, plus random ranges.
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for round in 0..40 {
        let mut again = vec![0u8; size];
        check!(
            volume.read("SH.ELF", 0, &mut again) == Ok(size) && again == whole,
            "whole re-read {round} differs"
        );
        let offset = (rng.next() as usize) % size;
        let len = 1 + (rng.next() as usize) % 20_000;
        let mut part = vec![0u8; len];
        let got = volume
            .read("SH.ELF", offset as u64, &mut part)
            .map_err(|error| format!("range {round}: {error:?}"))?;
        let end = (offset + len).min(size);
        check!(
            got == end - offset && part[..got] == whole[offset..end],
            "range {offset}+{len} differs from the whole read"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// PCI enumeration
// ---------------------------------------------------------------------------

/// The bus walk finds QEMU's host bridge at 00:00.0, is repeatable, and
/// `find_any` honours the caller's id priority in one pass.
fn pci_enumeration_is_stable_and_ranked() -> Result<(), String> {
    use crate::block::pci;
    let mut first = Vec::new();
    pci::for_each(|device| first.push(device));
    check!(!first.is_empty(), "no PCI function found");
    check!(
        first
            .iter()
            .any(|d| d.bus == 0 && d.device == 0 && d.function == 0),
        "the host bridge at 00:00.0 was not enumerated"
    );
    for _ in 0..50 {
        let mut again = 0usize;
        pci::for_each(|_| again += 1);
        check!(again == first.len(), "enumeration count changed: {again}");
    }

    let head = first[0];
    // Priority: an id that matches nothing, then the real one: still found.
    let found = pci::find_any(head.vendor, &[0xFFFE, head.id])
        .ok_or_else(|| String::from("find_any missed a present device"))?;
    check!(
        found.vendor == head.vendor && found.id == head.id,
        "find_any returned the wrong function"
    );
    check!(
        pci::find_any(0xDEAD, &[1, 2, 3]).is_none(),
        "find_any invented a device"
    );
    // Two present ids of one vendor: the earlier id in the list wins.
    let other = first
        .iter()
        .find(|d| d.vendor == head.vendor && d.id != head.id);
    if let Some(other) = other {
        let pick = pci::find_any(head.vendor, &[other.id, head.id]).map(|d| d.id);
        check!(pick == Some(other.id), "find_any ignored the id priority");
        let pick = pci::find_any(head.vendor, &[head.id, other.id]).map(|d| d.id);
        check!(pick == Some(head.id), "find_any ignored the id priority");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The lazy frame cursor
// ---------------------------------------------------------------------------

fn untouched_walks_regions_in_order() -> Result<(), String> {
    let mut starts = [0u64; crate::mem::MAX_REGIONS];
    let mut ends = [0u64; crate::mem::MAX_REGIONS];
    starts[0] = 0x10_0000;
    ends[0] = 0x10_0000 + 3 * 4096;
    starts[1] = 0x40_0000;
    ends[1] = 0x40_0000 + 4096 + 100; // one whole frame and a fragment
    let mut cursor = Untouched::new(&starts);
    let mut got = Vec::new();
    while let Some(frame) = cursor.next(&ends, 2) {
        got.push(frame);
    }
    check!(
        got == [0x10_0000, 0x10_1000, 0x10_2000, 0x40_0000],
        "frames handed out: {got:x?}"
    );
    check!(
        cursor.next(&ends, 2).is_none(),
        "the cursor must stay exhausted"
    );
    check!(
        Untouched::frames_in(0x10_0000, 0x10_0000 + 3 * 4096) == 3,
        "frames_in"
    );
    check!(
        Untouched::frames_in(0x40_0000, 0x40_0000 + 4196) == 1,
        "frames_in tail"
    );
    check!(
        Untouched::frames_in(10, 5) == 0,
        "frames_in must not underflow"
    );
    Ok(())
}

/// Soak the real allocator through the lazy path and the free list: many
/// frames come out distinct, go back, and are reused without leaking.
fn frame_allocator_lazy_soak() -> Result<(), String> {
    let before = mem::frame_stats();
    let mut held = Vec::new();
    for _ in 0..3000 {
        held.push(
            mem::alloc_frame()
                .ok_or_else(|| String::from("out of frames"))?
                .as_u64(),
        );
    }
    let mut sorted = held.clone();
    sorted.sort_unstable();
    sorted.dedup();
    check!(
        sorted.len() == held.len(),
        "the allocator handed out a frame twice"
    );
    check!(
        mem::frame_stats().live() == before.live() + 3000,
        "live frame count off after allocating"
    );
    for &frame in held.iter().rev() {
        mem::free_frame(x86_64::PhysAddr::new(frame));
    }
    let after = mem::frame_stats();
    check!(
        after.live() == before.live(),
        "frames leaked: {}",
        after.live() as i64 - before.live() as i64
    );
    check!(
        after.free + after.live() == after.total,
        "free + live != total after the soak"
    );
    // The most recently freed frame is the next one handed out.
    let again = mem::alloc_frame().ok_or_else(|| String::from("out of frames"))?;
    check!(again.as_u64() == held[0], "free-list reuse is not LIFO");
    mem::free_frame(again);
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "boot_io_ata_runs_match_single_reads",
        ata_runs_match_single_sector_reads,
    ),
    ("boot_io_ata_run_bounds", ata_run_bounds),
    (
        "boot_io_fat_fragmented_chain_reads",
        fat_fragmented_chain_reads,
    ),
    (
        "boot_io_fat_boot_volume_whole_vs_windows",
        fat_boot_volume_whole_matches_windows,
    ),
    (
        "boot_io_pci_enumeration",
        pci_enumeration_is_stable_and_ranked,
    ),
    ("boot_io_untouched_cursor", untouched_walks_regions_in_order),
    (
        "boot_io_frame_allocator_lazy_soak",
        frame_allocator_lazy_soak,
    ),
];
