use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use crate::bond::{hid_flag, MAX_DESCRIPTOR, MAX_NAME, MAX_REPORT_HANDLES};
use crate::fuzz::seal;
use crate::{
    AddrKind, BdAddr, Bond, BondStore, Error, Gatt, InputClass, LinkKey, Ltk, ReportHandle,
    MAX_BONDS,
};

pub fn keyboard() -> Bond {
    Bond {
        addr: BdAddr([1, 2, 3, 4, 5, 6]),
        kind: AddrKind::BrEdr,
        authenticated: true,
        secure_connections: false,
        key_size: 16,
        link_key: Some(LinkKey {
            key: [0x11; 16],
            key_type: 5,
        }),
        ltk: None,
        irk: None,
        class_of_device: 0x0005C0,
        input: InputClass::Keyboard,
        hid_flags: hid_flag::RECONNECT_INITIATE | hid_flag::BOOT_DEVICE,
        name: "Test Keyboard".to_string(),
        report_descriptor: (0..=255u8).cycle().take(300).collect(),
        gatt: Gatt::default(),
    }
}

pub fn mouse() -> Bond {
    Bond {
        addr: BdAddr([9, 8, 7, 6, 5, 4]),
        kind: AddrKind::BrEdr,
        authenticated: false,
        link_key: Some(LinkKey {
            key: [0x22; 16],
            key_type: 4,
        }),
        input: InputClass::Pointer,
        hid_flags: hid_flag::NORMALLY_CONNECTABLE,
        name: "Mouse".to_string(),
        report_descriptor: vec![0x05, 0x01, 0x09, 0x02],
        ..keyboard()
    }
}

pub fn le_keyboard() -> Bond {
    Bond {
        addr: BdAddr([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xF0]),
        kind: AddrKind::LeRandom,
        secure_connections: true,
        key_size: 16,
        link_key: None,
        ltk: Some(Ltk {
            key: [0x33; 16],
            ediv: 0x1234,
            rand: 0x0102_0304_0506_0708,
        }),
        irk: Some([0x44; 16]),
        class_of_device: 0,
        name: "LE Board".to_string(),
        gatt: Gatt {
            report_map: 0x0020,
            protocol_mode: 0x0024,
            reports: vec![
                ReportHandle {
                    value: 0x0030,
                    cccd: 0x0031,
                    report_id: 1,
                    report_type: 1,
                },
                ReportHandle {
                    value: 0x0034,
                    cccd: 0x0035,
                    report_id: 2,
                    report_type: 1,
                },
            ],
        },
        ..keyboard()
    }
}

fn sample() -> BondStore {
    let mut s = BondStore::new();
    for b in [keyboard(), mouse(), le_keyboard()] {
        s.upsert(b).unwrap();
    }
    s
}

#[test]
fn crc32_matches_the_check_value() {
    assert_eq!(crate::crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crate::crc32(b""), 0);
}

#[test]
fn round_trip_keeps_every_field() {
    let s = sample();
    let bytes = s.encode().unwrap();
    assert_eq!(&bytes[..4], b"LZBT");
    assert_eq!(&bytes[4..8], [1, 0, 3, 0]);
    let back = BondStore::decode(&bytes).unwrap();
    assert_eq!(back, s);
    let kb = back.find(keyboard().addr, AddrKind::BrEdr).unwrap();
    assert_eq!(kb.report_descriptor.len(), 300);
    assert!(kb.authenticated);
    let le = back.find(le_keyboard().addr, AddrKind::LeRandom).unwrap();
    assert_eq!(le.ltk.as_ref().unwrap().ediv, 0x1234);
    assert_eq!(le.gatt.reports.len(), 2);
    assert_eq!(le.irk, Some([0x44; 16]));
    assert_eq!(back.encode().unwrap(), bytes, "canonical");
}

#[test]
fn an_empty_store_round_trips() {
    let bytes = BondStore::new().encode().unwrap();
    assert_eq!(bytes.len(), 12);
    assert_eq!(BondStore::decode(&bytes), Ok(BondStore::new()));
}

#[test]
fn every_single_bit_flip_is_detected() {
    let bytes = sample().encode().unwrap();
    for bit in 0..bytes.len() * 8 {
        let mut b = bytes.clone();
        b[bit / 8] ^= 1 << (bit % 8);
        assert!(BondStore::decode(&b).is_err(), "bit {bit}");
    }
}

#[test]
fn every_truncation_is_refused() {
    let bytes = sample().encode().unwrap();
    for n in 0..bytes.len() {
        assert!(BondStore::decode(&bytes[..n]).is_err(), "len {n}");
    }
    let mut longer = bytes.clone();
    longer.push(0);
    assert!(BondStore::decode(&longer).is_err());
}

fn resealed(mut b: Vec<u8>) -> Vec<u8> {
    seal(&mut b);
    b
}

#[test]
fn header_checks() {
    let good = sample().encode().unwrap();
    let mut bad_magic = good.clone();
    bad_magic[0] = b'X';
    assert_eq!(BondStore::decode(&resealed(bad_magic)), Err(Error::Magic));
    let mut v2 = good.clone();
    v2[4] = 2;
    assert_eq!(BondStore::decode(&resealed(v2)), Err(Error::Version(2)));
    let mut crc = good.clone();
    let n = crc.len();
    crc[n - 1] ^= 1;
    assert_eq!(BondStore::decode(&crc), Err(Error::Checksum));
    let mut many = good.clone();
    many[6] = (MAX_BONDS + 1) as u8;
    assert_eq!(BondStore::decode(&resealed(many)), Err(Error::TooLarge));
    assert_eq!(
        BondStore::decode(&vec![0; crate::MAX_FILE + 1]),
        Err(Error::TooLarge)
    );
}

#[test]
fn a_count_that_disagrees_with_the_records_is_refused() {
    let good = sample().encode().unwrap();
    let mut fewer = good.clone();
    fewer[6] = 2;
    assert_eq!(BondStore::decode(&resealed(fewer)), Err(Error::Trailing));
    let mut more = good;
    more[6] = 4;
    assert_eq!(BondStore::decode(&resealed(more)), Err(Error::Truncated));
}

/// Offsets inside the first record's body: after the 8 byte header and its
/// 2 byte length.
const BODY: usize = 10;

fn first_record(mutate: impl Fn(&mut Vec<u8>)) -> Result<BondStore, Error> {
    let mut s = BondStore::new();
    s.upsert(keyboard()).unwrap();
    let mut bytes = s.encode().unwrap();
    mutate(&mut bytes);
    BondStore::decode(&resealed(bytes))
}

#[test]
fn hostile_record_fields_are_refused() {
    // Unknown flag bit.
    assert!(matches!(
        first_record(|b| b[BODY + 7] |= 0x80),
        Err(Error::Invalid(_))
    ));
    // Address kind out of range.
    assert!(matches!(
        first_record(|b| b[BODY + 6] = 9),
        Err(Error::Invalid(_))
    ));
    // Input class out of range.
    assert!(matches!(
        first_record(|b| b[BODY + 6 + 1 + 1 + 1 + 1 + 3] = 7),
        Err(Error::Invalid(_))
    ));
    // Key size out of range.
    assert!(matches!(
        first_record(|b| b[BODY + 8] = 5),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        first_record(|b| b[BODY + 8] = 17),
        Err(Error::Invalid(_))
    ));
    // LTK flag on a BR/EDR bond.
    assert!(matches!(
        first_record(|b| b[BODY + 7] |= 8),
        Err(Error::Invalid(_))
    ));
    // Unknown HID flag bits.
    assert!(matches!(
        first_record(|b| b[BODY + 6 + 1 + 1 + 1 + 1 + 3 + 1] = 0x80),
        Err(Error::Invalid(_))
    ));
    // A record length that is too big, or short of its fields.
    assert_eq!(
        first_record(|b| b[8..10].copy_from_slice(&u16::MAX.to_le_bytes())),
        Err(Error::TooLarge)
    );
    assert!(first_record(|b| b[8] -= 1).is_err());
    assert!(first_record(|b| b[8] += 1).is_err());
}

#[test]
fn stale_bytes_of_absent_keys_are_refused() {
    // Clear the "has a link key" flag but leave the key bytes behind: either
    // way the record is wrong ("no key" or stale bytes).
    assert!(first_record(|b| b[BODY + 7] &= !4).is_err());
    let mut s = BondStore::new();
    let mut le = le_keyboard();
    le.irk = None;
    s.upsert(le).unwrap();
    let mut bytes = s.encode().unwrap();
    // The IRK field sits 16 bytes before the name length octet.
    let irk_at = BODY + 6 + 4 + 3 + 2 + 16 + 16 + 2 + 8;
    bytes[irk_at] = 1;
    assert!(matches!(
        BondStore::decode(&resealed(bytes)),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn name_and_descriptor_lengths_are_bounded() {
    let name_len_at = BODY + 6 + 4 + 3 + 2 + 16 * 3 + 10;
    assert_eq!(first_record(|b| b[name_len_at] = 255), Err(Error::TooLarge));
    // A name longer than the record holds runs off the end.
    assert!(first_record(|b| b[name_len_at] = 200).is_err());
}

#[test]
fn non_utf8_names_are_refused() {
    let name_at = BODY + 6 + 4 + 3 + 2 + 16 * 3 + 10 + 1;
    assert!(matches!(
        first_record(|b| b[name_at] = 0xFF),
        Err(Error::Invalid("name"))
    ));
}

#[test]
fn duplicates_are_corruption_but_upsert_replaces() {
    let mut s = BondStore::new();
    s.upsert(keyboard()).unwrap();
    let one = s.encode().unwrap();
    // Two copies of the same record.
    let rec = &one[8..one.len() - 4];
    let mut twice = one[..8].to_vec();
    twice[6] = 2;
    twice.extend_from_slice(rec);
    twice.extend_from_slice(rec);
    twice.extend_from_slice(&[0; 4]);
    assert_eq!(BondStore::decode(&resealed(twice)), Err(Error::Duplicate));
    // Re-pairing replaces.
    let mut kb = keyboard();
    kb.link_key = Some(LinkKey {
        key: [0x99; 16],
        key_type: 5,
    });
    s.upsert(kb).unwrap();
    assert_eq!(s.len(), 1);
    assert_eq!(
        s.find(keyboard().addr, AddrKind::BrEdr)
            .unwrap()
            .link_key
            .as_ref()
            .unwrap()
            .key,
        [0x99; 16]
    );
    // The same address as LE and BR/EDR are two bonds.
    let mut le = le_keyboard();
    le.addr = keyboard().addr;
    s.upsert(le).unwrap();
    assert_eq!(s.len(), 2);
    assert!(s.remove(keyboard().addr, AddrKind::BrEdr));
    assert!(!s.remove(keyboard().addr, AddrKind::BrEdr));
}

#[test]
fn the_store_and_the_bond_rules_are_bounded() {
    let mut s = BondStore::new();
    for i in 0..MAX_BONDS as u8 {
        let mut b = mouse();
        b.addr = BdAddr([i, 0, 0, 0, 0, 1]);
        s.upsert(b).unwrap();
    }
    let mut extra = mouse();
    extra.addr = BdAddr([0xFF; 6]);
    assert_eq!(s.upsert(extra), Err(Error::Full));
    // A full store round-trips within MAX_FILE.
    let bytes = s.encode().unwrap();
    assert!(bytes.len() <= crate::MAX_FILE);
    assert_eq!(BondStore::decode(&bytes).unwrap().len(), MAX_BONDS);

    let mut b = keyboard();
    b.name = "x".repeat(MAX_NAME + 1);
    assert_eq!(b.validate(), Err(Error::TooLarge));
    let mut b = keyboard();
    b.report_descriptor = vec![0; MAX_DESCRIPTOR + 1];
    assert_eq!(b.validate(), Err(Error::TooLarge));
    let mut b = le_keyboard();
    b.gatt.reports = vec![
        ReportHandle {
            value: 1,
            cccd: 2,
            report_id: 0,
            report_type: 1
        };
        MAX_REPORT_HANDLES + 1
    ];
    assert_eq!(b.validate(), Err(Error::TooLarge));
}

#[test]
fn the_largest_legal_bond_fits_the_record_bound() {
    let mut b = le_keyboard();
    b.name = "n".repeat(MAX_NAME);
    b.report_descriptor = vec![0xAB; MAX_DESCRIPTOR];
    b.gatt.reports = vec![
        ReportHandle {
            value: 1,
            cccd: 2,
            report_id: 3,
            report_type: 4
        };
        MAX_REPORT_HANDLES
    ];
    let mut s = BondStore::new();
    s.upsert(b).unwrap();
    let bytes = s.encode().unwrap();
    assert!(
        bytes.len() - 12 <= crate::MAX_RECORD + 2 && bytes.len() - 12 >= crate::MAX_RECORD - 100
    );
    assert_eq!(BondStore::decode(&bytes), Ok(s));
}

#[test]
fn bond_rules() {
    let mut b = keyboard();
    b.link_key = None;
    assert!(matches!(b.validate(), Err(Error::Invalid("no key"))));
    let mut b = le_keyboard();
    b.link_key = Some(LinkKey {
        key: [1; 16],
        key_type: 4,
    });
    assert!(b.validate().is_err());
    let mut b = keyboard();
    b.irk = Some([0; 16]);
    assert!(b.validate().is_err());
    let mut b = le_keyboard();
    b.key_size = 0;
    assert!(b.validate().is_err());
    let mut b = keyboard();
    b.class_of_device = 1 << 24;
    assert!(b.validate().is_err());
    assert!(BondStore::new()
        .upsert(Bond {
            link_key: None,
            ..mouse()
        })
        .is_err());
}

#[test]
fn debug_output_never_shows_a_key() {
    let text = alloc::format!("{:?}{:?}", keyboard(), le_keyboard());
    assert!(!text.contains("17,") && !text.contains("0x11") && !text.contains("0x33"));
    assert!(!text.contains("key:"), "{text}");
    assert!(
        text.contains("xx:xx:xx:xx"),
        "addresses are redacted too: {text}"
    );
}

#[test]
fn seeded_fuzz_runs() {
    let valid = sample().encode().unwrap();
    fuzzkit::for_seeds("btbond_fuzz_run", |_seed, rng| {
        let mut data = if rng.one_in(2) {
            valid.clone()
        } else {
            let n = rng.below(200) as usize;
            rng.bytes(n)
        };
        let flips = rng.below(6) as usize;
        rng.flip_bits(&mut data, flips);
        if rng.one_in(3) {
            let n = rng.below(data.len() as u64 + 1) as usize;
            data.truncate(n);
        }
        crate::fuzz::run(&data);
    });
}

fn seed(name: &str) -> Vec<u8> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/btbond");
    std::fs::read(dir.join(name)).unwrap()
}

/// The seeds come from an independent Python encoder; the Rust encoder must
/// write exactly the same bytes for the same bonds.
#[test]
fn the_checked_in_seeds_match_the_rust_encoder() {
    assert_eq!(seed("store_empty"), BondStore::new().encode().unwrap());
    let mut one = BondStore::new();
    one.upsert(keyboard()).unwrap();
    assert_eq!(seed("store_keyboard"), one.encode().unwrap());
    assert_eq!(seed("store_three"), sample().encode().unwrap());
    // And every seed runs through the fuzz entry point.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/btbond");
    for entry in std::fs::read_dir(dir).unwrap() {
        crate::fuzz::run(&std::fs::read(entry.unwrap().path()).unwrap());
    }
    assert_eq!(
        BondStore::decode(&seed("store_bad_crc")),
        Err(Error::Checksum)
    );
    // The wrapped one-bond body decodes once the entry point seals it.
    let mut wrapped = b"LZBT".to_vec();
    wrapped.extend_from_slice(&1u16.to_le_bytes());
    wrapped.extend_from_slice(&seed("body_mouse"));
    wrapped.extend_from_slice(&[0; 4]);
    seal(&mut wrapped);
    assert_eq!(BondStore::decode(&wrapped).unwrap().len(), 1);
}
