//! MBR partition devices (docs/filesystem-plan.md F1): parsing hostile
//! tables, delegation with bounds, registration, and a read/write soak.

use super::block_suite::FakeDisk;
use super::*;
use crate::block::partition::{self, parse_mbr, Partition};
use crate::block::{self, BlockDevice, BlockError, SECTOR_SIZE};
use crate::fs::ext2::Ext2;
use alloc::boxed::Box;
use alloc::vec;

const DISK_SECTORS: u64 = 1280;

/// One blank disk shared by the tests that do not register it: a `FakeDisk` is
/// leaked, and the test heap is small.
fn shared(name: &'static str) -> &'static FakeDisk {
    static DISK: spin::Once<&'static FakeDisk> = spin::Once::new();
    let disk = DISK.call_once(|| FakeDisk::new(name, DISK_SECTORS as usize));
    disk.data.lock().fill(0);
    disk
}

/// One 16-byte MBR entry.
fn entry(kind: u8, lba: u32, sectors: u32) -> [u8; 16] {
    let mut raw = [0u8; 16];
    raw[4] = kind;
    raw[8..12].copy_from_slice(&lba.to_le_bytes());
    raw[12..16].copy_from_slice(&sectors.to_le_bytes());
    raw
}

/// An MBR sector holding `entries` (up to four) and the 0x55AA signature.
fn mbr(entries: &[[u8; 16]]) -> [u8; SECTOR_SIZE] {
    let mut sector = [0u8; SECTOR_SIZE];
    for (slot, raw) in entries.iter().enumerate() {
        sector[446 + slot * 16..462 + slot * 16].copy_from_slice(raw);
    }
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn used(table: &[Option<partition::Entry>; 4]) -> Vec<u8> {
    table.iter().flatten().map(|entry| entry.index).collect()
}

/// A bootloader-shaped table (stage 2, FAT, ext2) yields entries 2 and 3 only.
pub fn valid_table_registers_fat_and_ext2() -> Result<(), String> {
    let sector = mbr(&[
        entry(0x83, 1, 63),
        entry(0x0C, 100, 200),
        entry(0x83, 400, 800),
    ]);
    let table = parse_mbr(&sector, DISK_SECTORS, "t");
    check!(used(&table) == [1, 2, 3], "entries {:?}", used(&table));
    let third = table[2].ok_or("entry 3 missing")?;
    check!(
        third.lba == 400 && third.sectors == 800,
        "geometry {third:?}"
    );
    Ok(())
}

/// Every way an entry can lie about its extent is dropped, never clamped.
pub fn hostile_entries_are_skipped() -> Result<(), String> {
    let cases: &[(&str, [u8; 16])] = &[
        ("past the end", entry(0x83, 1200, 200)),
        ("lba zero", entry(0x83, 0, 100)),
        ("zero length", entry(0x83, 10, 0)),
        ("u32 overflow", entry(0x83, 0xFFFF_FFF0, 0x20)),
        ("extended", entry(0x05, 10, 100)),
        ("extended lba", entry(0x0F, 10, 100)),
        ("gpt protective", entry(0xEE, 1, 1279)),
        ("unknown type", entry(0x82, 10, 100)),
    ];
    for (what, raw) in cases {
        let table = parse_mbr(&mbr(&[*raw]), DISK_SECTORS, "t");
        check!(used(&table).is_empty(), "{what} was accepted: {table:?}");
    }
    // Missing signature: the whole table is ignored.
    let mut sector = mbr(&[entry(0x83, 10, 100)]);
    sector[510] = 0;
    check!(
        used(&parse_mbr(&sector, DISK_SECTORS, "t")).is_empty(),
        "a table without 0x55AA was used"
    );
    // The last sector of the disk is the last one allowed.
    let edge = parse_mbr(&mbr(&[entry(0x83, 1184, 96)]), DISK_SECTORS, "t");
    check!(
        used(&edge) == [1],
        "an extent ending at the disk end was refused"
    );
    Ok(())
}

/// Overlapping entries are both dropped; a disjoint third survives.
pub fn overlapping_entries_are_both_dropped() -> Result<(), String> {
    let sector = mbr(&[
        entry(0x83, 100, 100),
        entry(0x83, 150, 100),
        entry(0x83, 300, 50),
    ]);
    let table = parse_mbr(&sector, DISK_SECTORS, "t");
    check!(used(&table) == [3], "survivors {:?}", used(&table));
    // Adjacent (touching) extents do not overlap.
    let touching = parse_mbr(
        &mbr(&[entry(0x83, 100, 100), entry(0x83, 200, 100)]),
        DISK_SECTORS,
        "t",
    );
    check!(
        used(&touching) == [1, 2],
        "touching extents {:?}",
        used(&touching)
    );
    Ok(())
}

/// A disk with an MBR is not an ext2 volume, but the same bytes inside a
/// partition are.
pub fn ext2_refuses_a_partitioned_whole_disk() -> Result<(), String> {
    let disk = shared("pt-shared0");
    let image = super::ext2_suite::mkfs(1024, 512, 64);
    {
        let mut data = disk.data.lock();
        data[..SECTOR_SIZE].copy_from_slice(&mbr(&[entry(0x83, 128, 1024)]));
        data[128 * SECTOR_SIZE..128 * SECTOR_SIZE + image.len()].copy_from_slice(&image);
    }
    check!(
        Ext2::open(disk).is_err(),
        "ext2 opened a disk with a partition table"
    );
    let part = Box::leak(Box::new(Partition::new(disk, "pt-shared0p1", 128, 1024)));
    check!(
        Ext2::open(part).is_ok(),
        "ext2 did not open inside the partition"
    );
    Ok(())
}

/// Reads and writes are bounds-checked against the partition, translated by its
/// offset, and never touch the neighbouring partition.
pub fn partition_io_is_bounded_and_offset() -> Result<(), String> {
    let disk = shared("pt-shared0");
    let p2 = Partition::new(disk, "pt-shared0p2", 100, 200);
    let p3 = Partition::new(disk, "pt-shared0p3", 300, 50);
    let sector = [0xAB; SECTOR_SIZE];
    check!(
        p3.write_sectors(0, &sector).is_ok(),
        "write at the start failed"
    );
    check!(
        p3.write_sectors(49, &sector).is_ok(),
        "write at the last sector failed"
    );
    let mut raw = [0u8; SECTOR_SIZE];
    disk.read_sectors(300, &mut raw)
        .map_err(|e| format!("{e:?}"))?;
    check!(raw == sector, "p3 sector 0 did not land at disk LBA 300");
    disk.read_sectors(349, &mut raw)
        .map_err(|e| format!("{e:?}"))?;
    check!(raw == sector, "p3 sector 49 did not land at disk LBA 349");
    let mut buf = [0u8; SECTOR_SIZE];
    for lba in 0..200 {
        p2.read_sectors(lba, &mut buf)
            .map_err(|e| format!("{e:?}"))?;
        check!(
            buf == [0; SECTOR_SIZE],
            "p2 sector {lba} was touched by a write through p3"
        );
    }
    check!(
        p3.read_sectors(50, &mut buf) == Err(BlockError::Bounds),
        "read past the end"
    );
    check!(
        p3.write_sectors(50, &sector) == Err(BlockError::Bounds),
        "write past the end"
    );
    let mut two = [0u8; 2 * SECTOR_SIZE];
    check!(
        p3.read_sectors(49, &mut two) == Err(BlockError::Bounds),
        "straddling read"
    );
    check!(
        p3.write_sectors(u64::MAX, &sector) == Err(BlockError::Bounds),
        "huge lba write"
    );
    check!(
        p3.read_sectors(0, &mut buf[..100]) == Err(BlockError::Unsupported),
        "partial sector"
    );
    check!(
        p3.sector_count() == 50 && p3.is_partition() && p3.is_writable(),
        "metadata"
    );
    Ok(())
}

/// A scanned disk shows its partitions in the registry, once.
pub fn scan_registers_partitions() -> Result<(), String> {
    let disk = FakeDisk::new("pt-scan0", DISK_SECTORS as usize);
    disk.data.lock()[..SECTOR_SIZE]
        .copy_from_slice(&mbr(&[entry(0x01, 10, 20), entry(0x83, 100, 300)]));
    check!(block::register(disk).is_ok(), "registering the disk failed");
    partition::scan_disk(disk);
    let first = block::device("pt-scan0p1").ok_or("pt-scan0p1 missing")?;
    let second = block::device("pt-scan0p2").ok_or("pt-scan0p2 missing")?;
    check!(
        first.sector_count() == 20 && second.sector_count() == 300,
        "sizes"
    );
    check!(
        block::device("pt-scan0p3").is_none(),
        "an unused entry registered"
    );
    Ok(())
}

/// 100k random-offset reads and writes through a partition agree with a
/// reference buffer, and the surrounding disk is never disturbed.
pub fn partition_io_soak() -> Result<(), String> {
    const START: u64 = 200;
    const SECTORS: u64 = 800;
    let disk = shared("pt-shared0");
    let part = Partition::new(disk, "pt-shared0p4", START, SECTORS);
    let mut reference = vec![0u8; SECTORS as usize * SECTOR_SIZE];
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut buf = vec![0u8; 4 * SECTOR_SIZE];
    for round in 0..100_000u32 {
        let sectors = (next() % 4 + 1) as usize;
        let lba = next() % (SECTORS - sectors as u64 + 1);
        let bytes = sectors * SECTOR_SIZE;
        let at = lba as usize * SECTOR_SIZE;
        if next() % 2 == 0 {
            let fill = next() as u8;
            buf[..bytes].fill(fill);
            part.write_sectors(lba, &buf[..bytes])
                .map_err(|e| format!("{e:?}"))?;
            reference[at..at + bytes].fill(fill);
        } else {
            part.read_sectors(lba, &mut buf[..bytes])
                .map_err(|e| format!("{e:?}"))?;
            check!(
                buf[..bytes] == reference[at..at + bytes],
                "round {round}: read differs"
            );
        }
    }
    let data = disk.data.lock();
    let window = START as usize * SECTOR_SIZE..(START + SECTORS) as usize * SECTOR_SIZE;
    check!(
        data[window.clone()] == reference[..],
        "the partition window differs"
    );
    check!(
        data[..window.start].iter().all(|b| *b == 0) && data[window.end..].iter().all(|b| *b == 0),
        "a write escaped the partition"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("partition_valid_table", valid_table_registers_fat_and_ext2),
    ("partition_hostile_entries", hostile_entries_are_skipped),
    ("partition_overlap", overlapping_entries_are_both_dropped),
    (
        "partition_ext2_whole_disk_refused",
        ext2_refuses_a_partitioned_whole_disk,
    ),
    (
        "partition_io_bounds_and_offset",
        partition_io_is_bounded_and_offset,
    ),
    ("partition_scan_registers", scan_registers_partitions),
    ("partition_io_soak", partition_io_soak),
];
