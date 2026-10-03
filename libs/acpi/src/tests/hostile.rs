//! Hostile tables: each failure must cost only the feature that needed the
//! table, never a panic and never the other tables.

use std::vec::Vec;

use super::discover;
use super::golden::*;
use crate::fuzz::{seal, Image};
use crate::sdt::MAX_ROOT_ENTRIES;
use crate::{Error, Platform, Rsdp};

fn with_table(signature: &[u8; 4], edit: impl FnOnce(&mut Vec<u8>)) -> Image {
    let mut image = synthetic_modern();
    edit(image.table_mut(signature).expect("table"));
    image
}

#[test]
fn a_bad_hpet_checksum_costs_only_the_hpet() {
    let image = with_table(b"HPET", |t| t[40] ^= 1);
    let platform = discover(&image);
    assert_eq!(platform.hpet, Err(Error::BadChecksum));
    assert!(platform.fadt.is_ok() && platform.madt.is_ok());
}

#[test]
fn a_bad_fadt_costs_the_pm_timer_and_the_dsdt() {
    let image = with_table(b"FACP", |t| t[100] ^= 1);
    let platform = discover(&image);
    assert_eq!(platform.fadt, Err(Error::BadChecksum));
    assert_eq!(platform.dsdt, Err(Error::NotFound));
    assert!(platform.madt.is_ok() && platform.hpet.is_ok());
}

#[test]
fn a_bad_dsdt_is_reported_without_losing_the_fadt() {
    let image = with_table(b"DSDT", |t| t[50] ^= 1);
    let platform = discover(&image);
    assert_eq!(platform.dsdt, Err(Error::BadChecksum));
    assert!(platform.fadt.unwrap().pm_timer.is_some());
}

#[test]
fn madt_entry_lengths_must_add_up() {
    // Offsets 44.. are entries; the first is the 8-byte processor entry.
    for (len, why) in [
        (0u8, "zero"),
        (1, "one"),
        (7, "short"),
        (250, "past the end"),
    ] {
        let image = with_table(b"APIC", |t| {
            t[45] = len;
            seal(t);
        });
        let platform = discover(&image);
        assert_eq!(platform.madt, Err(Error::Malformed), "{why}");
        assert!(platform.fadt.is_ok(), "{why}");
    }
}

#[test]
fn madt_without_a_lapic_address_is_refused() {
    let image = with_table(b"APIC", |t| {
        t[36..40].copy_from_slice(&0xFEE0_0010u32.to_le_bytes());
        seal(t);
    });
    assert_eq!(discover(&image).madt, Err(Error::Malformed));
}

#[test]
fn hpet_outside_memory_space_is_refused() {
    let image = with_table(b"HPET", |t| {
        t[40] = 1; // I/O space
        seal(t);
    });
    assert_eq!(discover(&image).hpet, Err(Error::Malformed));
}

#[test]
fn table_lengths_are_bounded() {
    // Longer than the memory that holds it: unreadable, not a wild read.
    let image = with_table(b"APIC", |t| {
        let len = t.len() as u32 + 4;
        t[4..8].copy_from_slice(&len.to_le_bytes());
        seal(t);
    });
    assert_eq!(discover(&image).madt, Err(Error::Unreadable));
    // Absurd and too-small lengths are refused before any checksum walk.
    for len in [u32::MAX, 0x0100_0000, 35, 0] {
        let image = with_table(b"HPET", |t| t[4..8].copy_from_slice(&len.to_le_bytes()));
        assert_eq!(discover(&image).hpet, Err(Error::BadLength), "{len:#x}");
    }
}

#[test]
fn a_short_fadt_is_refused() {
    let mut image = synthetic_modern();
    *image.table_mut(b"FACP").unwrap() = table(b"FACP", 1, &[0u8; 40]);
    assert_eq!(discover(&image).fadt, Err(Error::BadLength));
}

#[test]
fn pm_timer_fields_are_validated() {
    // PM_TMR_LEN 3 with no X_ block: no PM timer. A port that would wrap past
    // 0xFFFF: no PM timer.
    let image = with_table(b"FACP", |t| {
        t[208..220].fill(0);
        t[91] = 3;
        seal(t);
    });
    assert_eq!(discover(&image).fadt.unwrap().pm_timer, None);
    let image = with_table(b"FACP", |t| {
        t[208 + 4..220].copy_from_slice(&0xFFFEu64.to_le_bytes());
        seal(t);
    });
    assert_eq!(discover(&image).fadt.unwrap().pm_timer, None);
    // Hardware-reduced ACPI has no PM timer whatever the blocks say.
    let image = with_table(b"FACP", |t| {
        t[112..116].copy_from_slice(&(1u32 << 20).to_le_bytes());
        seal(t);
    });
    let fadt = discover(&image).fadt.unwrap();
    assert!(fadt.hw_reduced() && fadt.pm_timer.is_none() && fadt.reset.is_none());
}

#[test]
fn the_first_valid_duplicate_wins() {
    let mut image = synthetic_modern();
    let mut broken = modern_hpet();
    broken[50] ^= 1;
    let mut other = modern_hpet();
    other[52] = 7; // HPET number 7
    seal(&mut other);
    image.segments.push((0x7FF1_0000, broken));
    image.segments.push((0x7FF1_1000, other));
    image.segments[1].1 = root(b"XSDT", &[0x7FF1_0000, FADT, 0x7FF1_1000, HPET, MADT]);
    let platform = discover(&image);
    assert_eq!(platform.hpet.unwrap().number, 7);
}

#[test]
fn bad_root_entries_are_counted_not_fatal() {
    let mut image = synthetic_modern();
    image.segments[1].1 = root(b"XSDT", &[0, 0xDEAD_0000, u64::MAX - 2, FADT]);
    let platform = discover(&image);
    assert_eq!(platform.bad_entries, 3);
    assert!(platform.fadt.is_ok());
    assert_eq!(platform.madt, Err(Error::NotFound));
}

#[test]
fn a_huge_root_is_capped() {
    let mut image = synthetic_modern();
    let entries: Vec<u64> = (0..1000).map(|i| 0x1_0000_0000 + i * 64).collect();
    image.segments[1].1 = root(b"XSDT", &entries);
    let platform = discover(&image);
    assert_eq!(u32::from(platform.bad_entries), MAX_ROOT_ENTRIES);
}

#[test]
fn an_invalid_xsdt_falls_back_to_the_rsdt() {
    let mut image = synthetic_modern();
    image.segments[1].1[20] ^= 1;
    let platform = discover(&image);
    assert_eq!(&platform.root.signature, b"RSDT");
    // The RSDT lists only the FADT.
    assert!(platform.fadt.is_ok());
    assert_eq!(platform.madt, Err(Error::NotFound));

    // A bad extended checksum ignores the XSDT pointer altogether.
    let mut image = synthetic_modern();
    image.segments[0].1[33] ^= 1;
    let rsdp = Rsdp::read(&image, RSDP).unwrap();
    assert_eq!(rsdp.xsdt, None);
    assert_eq!(&discover(&image).root.signature, b"RSDT");
}

#[test]
fn rsdp_failures_are_errors() {
    let image = synthetic_modern();
    assert_eq!(Rsdp::read(&image, 0).unwrap_err(), Error::NotFound);
    assert_eq!(
        Rsdp::read(&image, 0x1234_0000).unwrap_err(),
        Error::Unreadable
    );
    assert_eq!(Rsdp::read(&image, XSDT).unwrap_err(), Error::BadSignature);
    let mut bad = image.clone();
    bad.segments[0].1[10] ^= 1;
    assert_eq!(Rsdp::read(&bad, RSDP).unwrap_err(), Error::BadChecksum);
    // Neither root: nothing to walk.
    let mut none = image.clone();
    none.segments[0].1 = rsdp(0, 0);
    assert!(Platform::discover(&none, RSDP).is_err());
    // An XSDT only, and it is broken.
    let mut broken = image;
    broken.segments[0].1 = rsdp(0, XSDT);
    broken.segments[1].1[30] ^= 1;
    assert_eq!(
        Platform::discover(&broken, RSDP).unwrap_err(),
        Error::BadChecksum
    );
}

#[test]
fn reset_register_needs_the_flag_and_a_usable_space() {
    let image = with_table(b"FACP", |t| {
        t[112..116].copy_from_slice(&(1u32 << 8).to_le_bytes());
        seal(t);
    });
    assert_eq!(discover(&image).fadt.unwrap().reset, None);
    let image = with_table(b"FACP", |t| {
        t[116] = 0x7F; // functional fixed hardware
        seal(t);
    });
    assert_eq!(discover(&image).fadt.unwrap().reset, None);
}

#[test]
fn golden_dumps_survive_every_single_byte_corruption_of_their_fadt() {
    // Exhaustive over one table: flip each byte of q35's FADT, re-sealed and
    // not, and parse. Nothing may panic; the fuzz checker validates results.
    let image = dump("q35-seabios");
    let index = image
        .segments
        .iter()
        .position(|(_, d)| d.starts_with(b"FACP"))
        .unwrap();
    for at in 0..image.segments[index].1.len() {
        for sealed in [false, true] {
            let mut mutated = image.clone();
            mutated.segments[index].1[at] ^= 0xA5;
            let mut input = std::vec![u8::from(sealed)];
            input.extend_from_slice(&mutated.to_body());
            crate::fuzz::run(&input);
        }
    }
}
