//! The `\_S5` scan and the soft-off register values.

use std::vec::Vec;

use super::discover;
use super::golden::*;
use crate::s5::{find_s5, pm1_control_writes, pm1_enter_sequence, sleep_control_value, SleepType};
use crate::{Error, Platform};

/// A DSDT whose body is `body`, in the synthetic modern PC.
fn platform_with_dsdt(body: &[u8]) -> (crate::fuzz::Image, Platform) {
    let mut image = synthetic_modern();
    *image.table_mut(b"DSDT").expect("DSDT") = table(b"DSDT", 2, body);
    let platform = discover(&image);
    (image, platform)
}

fn scan(body: &[u8]) -> Result<SleepType, Error> {
    let (image, platform) = platform_with_dsdt(body);
    find_s5(&image, platform.dsdt.as_ref().expect("dsdt"))
}

/// `Name (\_S5, Package () { a, b, 0, 0 })` as iasl emits it.
fn s5(a: u8, b: u8) -> Vec<u8> {
    let mut v = std::vec![0x08, b'_', b'S', b'5', b'_', 0x12, 0x0A, 0x04];
    for value in [a, b] {
        v.extend_from_slice(&[0x0A, value]);
    }
    v.extend_from_slice(&[0x0A, 0x00, 0x0A, 0x00]);
    v
}

#[test]
fn every_golden_names_a_soft_off() {
    for (name, image) in all() {
        let platform = discover(&image);
        let sleep = find_s5(&image, platform.dsdt.as_ref().expect(name)).expect(name);
        // QEMU's PIIX4 and ICH9 power management both enter S5 with type 0.
        assert_eq!(sleep, SleepType { a: 0, b: 0 }, "{name}");
    }
}

#[test]
fn the_synthetic_modern_dsdt_has_one() {
    let platform = discover(&synthetic_modern());
    let sleep = find_s5(&synthetic_modern(), &platform.dsdt.unwrap()).unwrap();
    assert_eq!(sleep, SleepType { a: 0, b: 0 });
}

#[test]
fn real_firmware_encodings_decode() {
    // iasl's usual form (byte constants), Intel boards' 5/5 and 7/7, and the
    // ZeroOp / OneOp short forms.
    assert_eq!(scan(&s5(5, 5)), Ok(SleepType { a: 5, b: 5 }));
    assert_eq!(scan(&s5(7, 3)), Ok(SleepType { a: 7, b: 3 }));
    let short = [
        0x08, b'_', b'S', b'5', b'_', 0x12, 0x06, 0x04, 0x01, 0x00, 0x00, 0x00,
    ];
    assert_eq!(scan(&short), Ok(SleepType { a: 1, b: 0 }));
    // With the root prefix between NameOp and the name.
    let rooted = [
        0x08, 0x5C, b'_', b'S', b'5', b'_', 0x12, 0x08, 0x04, 0x0A, 0x05, 0x0A, 0x05, 0, 0,
    ];
    assert_eq!(scan(&rooted), Ok(SleepType { a: 5, b: 5 }));
    // A two-byte PkgLength (lead byte 0x40..=0x7F).
    let wide = [
        0x08, b'_', b'S', b'5', b'_', 0x12, 0x41, 0x00, 0x04, 0x0A, 0x04, 0x0A, 0x04, 0, 0,
    ];
    assert_eq!(scan(&wide), Ok(SleepType { a: 4, b: 4 }));
}

#[test]
fn the_name_is_found_at_every_alignment_of_the_scan_window() {
    // Positions around the 512-byte stride, where the context before the name
    // and the package after it straddle two reads.
    for pad in 0..1100usize {
        let mut body = std::vec![0xA5u8; pad];
        body.extend_from_slice(&s5(5, 6));
        body.extend_from_slice(&[0xA5; 9]);
        assert_eq!(scan(&body), Ok(SleepType { a: 5, b: 6 }), "pad {pad}");
    }
}

#[test]
fn it_is_found_at_the_very_end_of_a_big_dsdt() {
    let mut body = std::vec![0u8; 3 * 1024 * 1024];
    let end = body.len() - 12;
    body[end..].copy_from_slice(&s5(3, 3)[..12]);
    // The package is complete only up to its two constants.
    body.extend_from_slice(&[0, 0]);
    assert_eq!(scan(&body), Ok(SleepType { a: 3, b: 3 }));
}

#[test]
fn only_a_named_package_counts() {
    // The bytes of a name without NameOp (a string, a method's argument).
    let mut no_op = s5(5, 5);
    no_op[0] = 0x09;
    assert_eq!(scan(&no_op), Err(Error::Malformed));
    // NameOp but not a package.
    let mut not_package = s5(5, 5);
    not_package[5] = 0x11;
    assert_eq!(scan(&not_package), Err(Error::Malformed));
    // Fewer than two elements.
    let mut one = s5(5, 5);
    one[7] = 1;
    assert_eq!(scan(&one), Err(Error::Malformed));
    // A sleep type is three bits.
    assert_eq!(scan(&s5(8, 0)), Err(Error::Malformed));
    assert_eq!(scan(&s5(0, 0xFF)), Err(Error::Malformed));
    // Elements that are not small constants (a buffer).
    let buffer = [
        0x08, b'_', b'S', b'5', b'_', 0x12, 0x08, 0x04, 0x11, 0x03, 0x0A, 0x05, 0x00,
    ];
    assert_eq!(scan(&buffer), Err(Error::Malformed));
    // No such name at all.
    assert_eq!(scan(&[0u8; 100]), Err(Error::NotFound));
}

#[test]
fn a_bad_candidate_does_not_hide_a_good_one() {
    let mut body = s5(9, 9);
    body.extend_from_slice(&[0; 7]);
    body.extend_from_slice(&s5(5, 5));
    assert_eq!(scan(&body), Ok(SleepType { a: 5, b: 5 }));
}

#[test]
fn a_package_cut_off_by_the_end_of_the_table_is_malformed() {
    let full = s5(5, 5);
    for cut in 5..full.len() - 4 {
        let result = scan(&full[..cut]);
        assert!(result == Err(Error::Malformed), "cut {cut}: {result:?}");
    }
}

#[test]
fn an_unreadable_dsdt_is_reported() {
    let (_, platform) = platform_with_dsdt(&s5(5, 5));
    let empty = crate::fuzz::Image {
        rsdp: 0,
        segments: Vec::new(),
    };
    assert_eq!(
        find_s5(&empty, platform.dsdt.as_ref().unwrap()),
        Err(Error::Unreadable)
    );
}

#[test]
fn pm1_control_writes_keep_the_other_bits() {
    // SCI_EN (bit 0) stays; stale SLP_TYP and SLP_EN are replaced.
    let (prepared, enter) = pm1_control_writes(0x3C01, 5);
    assert_eq!(prepared, 0x1401);
    assert_eq!(enter, 0x3401);
    assert_eq!(pm1_control_writes(0, 0), (0, 0x2000));
    // GBL_RLS is write-only: a stale 1 read back there is not written again.
    assert_eq!(pm1_control_writes(0x0005, 3), (0x0C01, 0x2C01));
    // A type wider than three bits cannot spill into SLP_EN.
    assert_eq!(pm1_control_writes(0, 0xFF).0, 0x1C00);
}

#[test]
fn the_sleep_control_value_places_type_and_enable() {
    assert_eq!(sleep_control_value(0), 0x20);
    assert_eq!(sleep_control_value(5), 0x34);
    assert_eq!(sleep_control_value(0xFF), 0x3C);
}

#[test]
fn the_fadt_exposes_the_sleep_control_register_only_when_present() {
    let platform = discover(&synthetic_modern());
    // The synthetic FADT is a revision 6 table with the block left zero.
    assert_eq!(platform.fadt.unwrap().sleep_control, None);
}

#[test]
fn the_enter_sequence_sets_the_type_everywhere_before_enabling_anywhere() {
    // PM1a only.
    let (writes, used) = pm1_enter_sequence(&[(0x0001, 5)]);
    assert_eq!(&writes[..used], [(0, 0x1401), (0, 0x3401)]);
    // PM1a and PM1b: each block is read-modify-written from its own value
    // with its own SLP_TYP, SLP_TYP in both blocks before SLP_EN in either.
    let (writes, used) = pm1_enter_sequence(&[(0x0001, 5), (0x0000, 6)]);
    assert_eq!(
        &writes[..used],
        [(0, 0x1401), (1, 0x1800), (0, 0x3401), (1, 0x3800)]
    );
    // No blocks, no writes; more than two are ignored.
    assert_eq!(pm1_enter_sequence(&[]).1, 0);
    assert_eq!(pm1_enter_sequence(&[(0, 0); 5]).1, 4);
}

#[test]
fn word_constants_decode_when_the_high_byte_is_zero() {
    let word = [
        0x08, b'_', b'S', b'5', b'_', 0x12, 0x0A, 0x04, 0x0B, 0x05, 0x00, 0x0B, 0x06, 0x00, 0, 0,
    ];
    assert_eq!(scan(&word), Ok(SleepType { a: 5, b: 6 }));
    let wide = [
        0x08, b'_', b'S', b'5', b'_', 0x12, 0x0A, 0x04, 0x0B, 0x05, 0x01, 0x0B, 0x06, 0x00, 0, 0,
    ];
    assert_eq!(scan(&wide), Err(Error::Malformed));
}
