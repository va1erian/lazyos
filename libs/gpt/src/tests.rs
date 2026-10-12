//! Host tests: the CRC and GUID forms against known values, tables written
//! by [`crate::write`] read back, every way a copy can be broken, and seeded
//! fuzz of the shared entry point.

use std::vec;
use std::vec::Vec;

use crate::crc::crc32;
use crate::entry::EntryError;
use crate::fuzz::MemDisk;
use crate::guid::{Guid, ESP, LINUX_DATA};
use crate::header::{Header, HeaderError};
use crate::write::{self, Spec, ARRAY_SECTORS, FIRST_USABLE};
use crate::{is_protective, read, CopyError, Source, SECTOR};

const DISK: u64 = 4096;

fn spec(first: u64, last: u64, name: &'static str) -> Spec<'static> {
    Spec {
        type_guid: LINUX_DATA,
        unique_guid: Guid([first as u8; 16]),
        first_lba: first,
        last_lba: last,
        name,
    }
}

/// The installed layout of docs/grub-install-plan.md: ESP, Linux root, the
/// two LazyOS partitions.
fn linux_box() -> Vec<u8> {
    let mut esp = spec(64, 263, "EFI System Partition");
    esp.type_guid = ESP;
    let parts = [
        esp,
        spec(264, 1999, ""),
        spec(2000, 2999, "lazyos-system"),
        spec(3000, 3999, "lazyos-home"),
    ];
    let layout = write::build(DISK, Guid([9; 16]), &parts).unwrap();
    let mut disk = vec![0u8; DISK as usize * SECTOR];
    layout.apply(&mut disk);
    disk
}

fn table(disk: &[u8]) -> Result<crate::Table, crate::Failure> {
    read(&mut MemDisk(disk), (disk.len() / SECTOR) as u64)
}

/// Rewrite the primary header after `edit`, with a correct CRC.
fn edit_primary(disk: &mut [u8], edit: impl FnOnce(&mut Header)) {
    let sector: &[u8; SECTOR] = disk[SECTOR..2 * SECTOR].try_into().unwrap();
    let mut header = Header::parse(sector, 1, DISK).unwrap();
    edit(&mut header);
    disk[SECTOR..2 * SECTOR].copy_from_slice(&header.to_sector());
}

#[test]
fn crc_known_vector() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(b""), 0);
}

#[test]
fn guid_text_round_trip() {
    let text = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";
    assert_eq!(ESP.0[..4], [0x28, 0x73, 0x2A, 0xC1]);
    assert_eq!(std::format!("{ESP}"), text);
    assert_eq!(Guid::parse(&text.to_lowercase()), Some(ESP));
    for bad in [
        "",
        "C12A7328F81F11D2BA4B00A0C93EC93B",
        "G12A7328-F81F-11D2-BA4B-00A0C93EC93B",
    ] {
        assert_eq!(Guid::parse(bad), None, "{bad}");
    }
    assert_eq!(ESP.type_name(), Some("esp"));
}

#[test]
fn written_table_reads_back() {
    let disk = linux_box();
    assert!(is_protective(disk[..SECTOR].try_into().unwrap()));
    let table = table(&disk).unwrap();
    assert_eq!(table.source, Source::Primary);
    assert!(table.rejected.is_empty());
    let indexes: Vec<u32> = table.entries.iter().map(|e| e.index).collect();
    assert_eq!(indexes, [1, 2, 3, 4]);
    let system = table.by_name("lazyos-system").unwrap();
    assert_eq!(
        (system.index, system.first_lba, system.sectors()),
        (3, 2000, 1000)
    );
    assert_eq!(table.entries[0].type_guid, ESP);
    assert!(table.by_name("lazyos").is_none());
    assert_eq!(table.header.last_usable, DISK - 2 - ARRAY_SECTORS);
}

#[test]
fn hybrid_mbr_is_protective() {
    let mut mbr = write::protective_mbr(DISK);
    mbr[462 + 4] = 0x0C; // a FAT entry beside the 0xEE one
    assert!(is_protective(&mbr));
    mbr[446 + 4] = 0x83;
    assert!(!is_protective(&mbr));
    let mut unsigned = write::protective_mbr(DISK);
    unsigned[510] = 0;
    assert!(!is_protective(&unsigned));
}

#[test]
fn bad_primary_header_uses_backup() {
    let mut disk = linux_box();
    disk[SECTOR + 40] ^= 1; // first_usable, CRC now wrong
    let table = table(&disk).unwrap();
    assert_eq!(table.source, Source::Backup);
    assert_eq!(
        table.primary_error,
        Some(CopyError::Header(HeaderError::Crc))
    );
    assert_eq!(table.entries.len(), 4);
}

#[test]
fn bad_primary_array_uses_backup() {
    let mut disk = linux_box();
    disk[2 * SECTOR + 32] ^= 0x80; // entry 1's first LBA
    let table = table(&disk).unwrap();
    assert_eq!(table.source, Source::Backup);
    assert_eq!(table.primary_error, Some(CopyError::ArrayCrc));
}

#[test]
fn both_copies_bad_fail() {
    let mut disk = linux_box();
    disk[SECTOR] = b'X';
    let last = (DISK as usize - 1) * SECTOR;
    disk[last + 16] ^= 1;
    let failure = table(&disk).unwrap_err();
    assert_eq!(failure.primary, CopyError::Header(HeaderError::Signature));
    assert_eq!(failure.backup, CopyError::Header(HeaderError::Crc));
}

#[test]
fn hostile_headers_are_refused() {
    type Case = (&'static str, fn(&mut Header), HeaderError);
    let cases: [Case; 10] = [
        (
            "alternate inside the disk's front",
            |h| h.alternate_lba = 2,
            HeaderError::Bounds,
        ),
        ("my_lba", |h| h.my_lba = 2, HeaderError::Location),
        (
            "last usable past the disk",
            |h| h.last_usable = DISK,
            HeaderError::Bounds,
        ),
        (
            "inverted usable",
            |h| h.first_usable = h.last_usable + 1,
            HeaderError::Bounds,
        ),
        (
            "usable holds the header",
            |h| h.first_usable = 1,
            HeaderError::Bounds,
        ),
        (
            "array inside usable",
            |h| h.entries_lba = 100,
            HeaderError::Bounds,
        ),
        (
            "array past the disk",
            |h| h.entries_lba = u64::MAX - 1,
            HeaderError::Bounds,
        ),
        ("entry size 100", |h| h.entry_size = 100, HeaderError::Array),
        ("entry size 384", |h| h.entry_size = 384, HeaderError::Array),
        (
            "too many entries",
            |h| h.entry_count = 100_000,
            HeaderError::Array,
        ),
    ];
    for (what, edit, want) in cases {
        let mut disk = linux_box();
        edit_primary(&mut disk, edit);
        let sector: &[u8; SECTOR] = disk[SECTOR..2 * SECTOR].try_into().unwrap();
        assert_eq!(Header::parse(sector, 1, DISK), Err(want), "{what}");
        // The backup still carries the disk.
        assert_eq!(table(&disk).unwrap().source, Source::Backup, "{what}");
    }
}

#[test]
fn backup_must_point_at_lba_1_and_a_grown_disk_still_reads() {
    // A backup naming anything but LBA 1 is refused.
    let mut disk = linux_box();
    let last = (DISK as usize - 1) * SECTOR;
    let sector: &[u8; SECTOR] = disk[last..].try_into().unwrap();
    let mut backup = Header::parse(sector, DISK - 1, DISK).unwrap();
    backup.alternate_lba = 7;
    disk[last..].copy_from_slice(&backup.to_sector());
    let sector: &[u8; SECTOR] = disk[last..].try_into().unwrap();
    assert_eq!(
        Header::parse(sector, DISK - 1, DISK),
        Err(HeaderError::Bounds)
    );
    // The same disk grown by 1 MiB: the primary still names the old end.
    let mut grown = linux_box();
    grown.resize(grown.len() + (1 << 20), 0);
    let table = table(&grown).unwrap();
    assert_eq!(table.source, Source::Primary);
    assert_eq!(table.entries.len(), 4);
}

#[test]
fn hostile_entries_are_dropped() {
    let mut disk = linux_box();
    // Entry 2 swallows entry 3; entry 4 runs past the usable area.
    let entry = |n: usize| 2 * SECTOR + (n - 1) * 128;
    disk[entry(2) + 40..entry(2) + 48].copy_from_slice(&2500u64.to_le_bytes());
    disk[entry(4) + 40..entry(4) + 48].copy_from_slice(&(DISK - 2).to_le_bytes());
    let crc = crc32(&disk[2 * SECTOR..FIRST_USABLE as usize * SECTOR]);
    edit_primary(&mut disk, |h| h.entries_crc = crc);
    let table = table(&disk).unwrap();
    assert_eq!(table.source, Source::Primary);
    let used: Vec<u32> = table.entries.iter().map(|e| e.index).collect();
    assert_eq!(used, [1]);
    assert_eq!(
        table.rejected,
        [
            (2, EntryError::Overlap),
            (3, EntryError::Overlap),
            (4, EntryError::OutsideUsable)
        ]
    );
}

#[test]
fn full_table_of_128() {
    let names: Vec<std::string::String> = (0..128).map(|n| std::format!("p{n}")).collect();
    let parts: Vec<Spec> = names
        .iter()
        .enumerate()
        .map(|(n, name)| Spec {
            name,
            ..spec(
                FIRST_USABLE + n as u64 * 10,
                FIRST_USABLE + n as u64 * 10 + 9,
                "",
            )
        })
        .collect();
    let layout = write::build(DISK, Guid([1; 16]), &parts).unwrap();
    let mut disk = vec![0u8; DISK as usize * SECTOR];
    layout.apply(&mut disk);
    let table = table(&disk).unwrap();
    assert_eq!(table.entries.len(), 128);
    assert!(table.entries[127].name_is("p127"));
}

#[test]
fn writer_refuses_bad_specs() {
    use write::BuildError::*;
    let build = |parts: &[Spec]| write::build(DISK, Guid([1; 16]), parts).err();
    assert_eq!(build(&[spec(10, 20, "")]), Some(Extent(1)));
    assert_eq!(
        build(&[spec(100, 200, ""), spec(150, 300, "")]),
        Some(Overlap(2))
    );
    assert_eq!(build(&[spec(100, 99, "")]), Some(Extent(1)));
    assert_eq!(
        build(&[spec(100, 200, "x".repeat(37).leak())]),
        Some(Name(1))
    );
    assert_eq!(
        write::build(60, Guid([1; 16]), &[]).err(),
        Some(DiskTooSmall)
    );
}

#[test]
fn seeded_fuzz() {
    fuzzkit::for_seeds("gpt_fuzz", |_, rng| {
        let mut data = vec![rng.byte() % 2];
        data.extend(rng.bytes(64));
        crate::fuzz::run(&data);
    });
    fuzzkit::for_seeds("gpt_fuzz_raw", |_, rng| {
        let mut disk = linux_box();
        rng.flip_bits(&mut disk[..FIRST_USABLE as usize * SECTOR], 4);
        let mut data = vec![0u8];
        data.extend_from_slice(&disk);
        crate::fuzz::run(&data);
    });
}

#[test]
fn checked_in_fuzz_seeds_run_clean() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/gpt");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return; // a checkout without the fuzz corpus
    };
    for entry in entries.flatten() {
        let data = std::fs::read(entry.path()).unwrap();
        crate::fuzz::run(&data);
    }
}
