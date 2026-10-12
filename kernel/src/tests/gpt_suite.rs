//! GPT partition devices (docs/grub-install-plan.md K2): tables written by
//! `libs/gpt` read back through `block::gpt`, every copy broken in turn, the
//! hybrid-MBR fallback, registration as `<disk>p<n>` with GPT numbering, ext2
//! inside a GPT partition, and a soak of damaged tables.

use super::block_suite::FakeDisk;
use super::*;
use crate::block::gpt;
use crate::block::partition::{self, Entry, Partition, KIND_ESP, KIND_LINUX};
use crate::block::{self, BlockDevice, SECTOR_SIZE};
use crate::fs::ext2::Ext2;
use ::gpt::guid::{Guid, ESP, LINUX_DATA};
use ::gpt::write::{self, Spec};
use alloc::boxed::Box;

const DISK_SECTORS: u64 = 1024;
/// Linux swap: a type LazyOS never registers.
const SWAP: Guid = Guid::from_text("0657FD6D-A4AB-43C4-84E5-0933C84B4F4F");

/// One blank disk shared by the tests that do not register it (a `FakeDisk`
/// is leaked, and the test heap is small).
fn shared() -> &'static FakeDisk {
    static DISK: spin::Once<&'static FakeDisk> = spin::Once::new();
    let disk = DISK.call_once(|| FakeDisk::new("gpt-shared0", DISK_SECTORS as usize));
    disk.data.lock().fill(0);
    disk
}

fn spec(kind: Guid, first: u64, last: u64, name: &'static str) -> Spec<'static> {
    Spec {
        type_guid: kind,
        unique_guid: Guid([first as u8; 16]),
        first_lba: first,
        last_lba: last,
        name,
    }
}

/// The plan's layout in miniature: ESP, a Linux root, `lazyos-system`,
/// `lazyos-home`.
fn linux_box() -> [Spec<'static>; 4] {
    [
        spec(ESP, 40, 99, "EFI System Partition"),
        spec(LINUX_DATA, 100, 299, "ubuntu"),
        spec(LINUX_DATA, 300, 699, "lazyos-system"),
        spec(LINUX_DATA, 700, 959, "lazyos-home"),
    ]
}

fn write_table(disk: &FakeDisk, parts: &[Spec]) -> Result<(), String> {
    let layout = write::build(DISK_SECTORS, Guid([5; 16]), parts).map_err(|e| format!("{e:?}"))?;
    // `sector_count` takes the data lock itself: ask before taking it.
    let bytes = (disk.sector_count() as usize) * SECTOR_SIZE;
    layout.apply(&mut disk.data.lock()[..bytes]);
    Ok(())
}

fn sector0(disk: &FakeDisk) -> [u8; SECTOR_SIZE] {
    let mut mbr = [0u8; SECTOR_SIZE];
    mbr.copy_from_slice(&disk.data.lock()[..SECTOR_SIZE]);
    mbr
}

fn indexes(entries: &[Entry]) -> Vec<u32> {
    entries.iter().map(|entry| entry.index).collect()
}

/// A valid table yields every mountable entry, GPT-numbered, with its extent.
pub fn valid_table() -> Result<(), String> {
    let disk = shared();
    write_table(disk, &linux_box())?;
    let entries = gpt::entries(disk, &sector0(disk)).ok_or("no table")?;
    check!(indexes(&entries) == [1, 2, 3, 4], "{entries:?}");
    check!(
        entries[0].kind == KIND_ESP && entries[2].kind == KIND_LINUX,
        "kinds {entries:?}"
    );
    check!(
        entries[2].lba == 300 && entries[2].sectors == 400,
        "system extent {:?}",
        entries[2]
    );
    Ok(())
}

/// A broken primary header or array falls back to the backup copy.
pub fn bad_primary_uses_backup() -> Result<(), String> {
    for (what, offset) in [
        ("header", SECTOR_SIZE + 24),
        ("array", 2 * SECTOR_SIZE + 40),
    ] {
        let disk = shared();
        write_table(disk, &linux_box())?;
        disk.data.lock()[offset] ^= 0x10;
        let entries = gpt::entries(disk, &sector0(disk)).ok_or("no table")?;
        check!(indexes(&entries) == [1, 2, 3, 4], "{what}: {entries:?}");
    }
    Ok(())
}

/// With both copies broken the disk is read by its MBR: a hybrid MBR's own
/// entries survive, a pure protective MBR yields nothing.
pub fn both_bad_falls_back_to_mbr() -> Result<(), String> {
    let disk = shared();
    write_table(disk, &linux_box())?;
    {
        let mut data = disk.data.lock();
        data[SECTOR_SIZE] = b'X';
        let last = (DISK_SECTORS as usize - 1) * SECTOR_SIZE;
        data[last] = b'X';
        // Hybrid: MBR entry 2 maps the system partition.
        let entry = &mut data[462..478];
        entry[4] = 0x83;
        entry[8..12].copy_from_slice(&300u32.to_le_bytes());
        entry[12..16].copy_from_slice(&400u32.to_le_bytes());
    }
    let mbr = sector0(disk);
    check!(
        gpt::entries(disk, &mbr).is_none(),
        "a broken table was used"
    );
    let fallback: Vec<u32> = partition::parse_mbr(&mbr, DISK_SECTORS, "t")
        .iter()
        .flatten()
        .map(|entry| entry.index)
        .collect();
    check!(fallback == [2], "MBR fallback {fallback:?}");
    Ok(())
}

/// Entries past the usable area, overlapping, or of a type LazyOS never
/// mounts are not offered; the others still are.
pub fn hostile_and_foreign_entries() -> Result<(), String> {
    let disk = shared();
    let mut parts = linux_box();
    parts[1].type_guid = SWAP;
    write_table(disk, &parts)?;
    // Make entry 4 overlap entry 3 and fix the CRCs by rewriting through the
    // library: decode, edit, re-encode the primary header.
    {
        let mut data = disk.data.lock();
        let at = 2 * SECTOR_SIZE + 3 * 128 + 32;
        data[at..at + 8].copy_from_slice(&650u64.to_le_bytes());
        let array_end = (write::FIRST_USABLE as usize) * SECTOR_SIZE;
        let crc = ::gpt::crc::crc32(&data[2 * SECTOR_SIZE..array_end]);
        let sector: &[u8; SECTOR_SIZE] = data[SECTOR_SIZE..2 * SECTOR_SIZE].try_into().unwrap();
        let mut header =
            ::gpt::Header::parse(sector, 1, DISK_SECTORS).map_err(|e| format!("{e:?}"))?;
        header.entries_crc = crc;
        data[SECTOR_SIZE..2 * SECTOR_SIZE].copy_from_slice(&header.to_sector());
    }
    let entries = gpt::entries(disk, &sector0(disk)).ok_or("no table")?;
    check!(indexes(&entries) == [1], "{entries:?}");
    Ok(())
}

/// A scanned GPT disk registers `<disk>p<n>` by GPT index, two digits
/// included, and skips the types it never mounts.
pub fn scan_registers_gpt_names() -> Result<(), String> {
    let disk = FakeDisk::new("gpt-scan0", 512);
    let mut parts = alloc::vec![spec(ESP, 40, 59, "esp")];
    for n in 0..10u64 {
        parts.push(spec(SWAP, 60 + n, 60 + n, "swap"));
    }
    parts.push(spec(LINUX_DATA, 100, 299, "lazyos-system"));
    let layout = write::build(512, Guid([6; 16]), &parts).map_err(|e| format!("{e:?}"))?;
    layout.apply(&mut disk.data.lock()[..]);
    check!(block::register(disk).is_ok(), "registering the disk failed");
    let count = partition::scan_disk(disk);
    check!(count == 2, "scan_disk reported {count} partitions");
    let system = block::device("gpt-scan0p12").ok_or("gpt-scan0p12 missing")?;
    check!(
        system.sector_count() == 200,
        "size {}",
        system.sector_count()
    );
    check!(block::device("gpt-scan0p1").is_some(), "the ESP is missing");
    check!(block::device("gpt-scan0p2").is_none(), "swap registered");
    Ok(())
}

/// An ext2 volume inside a GPT partition opens there and not on the disk.
pub fn ext2_inside_gpt_partition() -> Result<(), String> {
    let disk = shared();
    write_table(disk, &[spec(LINUX_DATA, 64, 64 + 511, "lazyos-system")])?;
    // A 256-block volume; `mkfs` pads its buffer to the suite's disk size.
    let image = super::ext2_suite::mkfs(1024, 256, 32);
    let volume = &image[..256 * 1024];
    disk.data.lock()[64 * SECTOR_SIZE..64 * SECTOR_SIZE + volume.len()].copy_from_slice(volume);
    let entries = gpt::entries(disk, &sector0(disk)).ok_or("no table")?;
    let entry = entries.first().ok_or("no entry")?;
    check!(Ext2::open(disk).is_err(), "ext2 opened the whole GPT disk");
    let part = Box::leak(Box::new(Partition::new(
        disk,
        "gpt-shared0p1",
        entry.lba,
        entry.sectors,
    )));
    check!(
        Ext2::open(part).is_ok(),
        "ext2 did not open inside the partition"
    );
    Ok(())
}

/// Soak: 2000 rounds of random damage to both copies; whatever is offered
/// stays inside the disk and never overlaps, and the heap does not grow.
pub fn damage_soak() -> Result<(), String> {
    let disk = shared();
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let measure = || crate::mem::slab::stats().live_bytes + crate::mem::heap_stats().used;
    write_table(disk, &linux_box())?;
    let pristine: Vec<u8> = disk.data.lock().clone();
    let _ = gpt::entries(disk, &sector0(disk)); // warm caches
    let before = measure();
    let head = write::FIRST_USABLE as usize * SECTOR_SIZE;
    let tail = (write::last_usable(DISK_SECTORS) as usize + 1) * SECTOR_SIZE;
    for round in 0..2000u32 {
        {
            let mut data = disk.data.lock();
            data.copy_from_slice(&pristine);
            for _ in 0..(next() % 4 + 1) {
                let at = if next() % 2 == 0 {
                    SECTOR_SIZE + (next() as usize) % (head - SECTOR_SIZE)
                } else {
                    tail + (next() as usize) % (data.len() - tail)
                };
                data[at] = next() as u8;
            }
        }
        if let Some(entries) = gpt::entries(disk, &sector0(disk)) {
            for entry in &entries {
                check!(
                    entry.sectors > 0 && entry.lba + entry.sectors <= DISK_SECTORS,
                    "round {round}: {entry:?}"
                );
            }
            let clash = entries.iter().enumerate().any(|(i, a)| {
                entries[i + 1..]
                    .iter()
                    .any(|b| a.lba < b.lba + b.sectors && b.lba < a.lba + a.sectors)
            });
            check!(!clash, "round {round}: overlap {entries:?}");
        }
    }
    let after = measure();
    check!(
        after <= before + 16 * 1024,
        "2000 scans grew the heap from {before} to {after}"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("partition_gpt_valid", valid_table),
    ("partition_gpt_bad_primary_backup", bad_primary_uses_backup),
    (
        "partition_gpt_both_bad_mbr_fallback",
        both_bad_falls_back_to_mbr,
    ),
    ("partition_gpt_hostile_entries", hostile_and_foreign_entries),
    ("partition_gpt_scan_names", scan_registers_gpt_names),
    ("partition_gpt_ext2_inside", ext2_inside_gpt_partition),
    ("partition_gpt_damage_soak", damage_soak),
];
