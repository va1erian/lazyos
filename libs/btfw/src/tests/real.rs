//! The loaders against the real linux-firmware files, when they are present.
//!
//! `python tools/firmware/fetch.py --switch bt` leaves them under
//! `target/firmware/`; without them these tests say so and pass, since CI
//! does not download firmware. They are the check that the synthetic fixtures
//! in the other tests describe the real container layouts.

use std::path::PathBuf;
use std::vec::Vec;

use crate::intel::sfi::{Layout, Sfi};
use crate::realtek::epatch;

fn firmware(name: &str) -> Option<Vec<u8>> {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "../../target/firmware", name]
        .iter()
        .collect();
    let data = std::fs::read(path).ok();
    if data.is_none() {
        std::eprintln!("btfw: {name} not fetched, real-file check skipped");
    }
    data
}

#[test]
fn real_intel_images_parse_with_the_layout_their_generation_uses() {
    // AX201 (legacy bootloader): RSA header only, boot address 0x24800.
    if let Some(data) = firmware("intel/ibt-19-0-4.sfi") {
        let sfi = Sfi::parse(data, Layout::Rsa).unwrap();
        assert_eq!(sfi.boot_addr, 0x24800);
        assert_eq!(sfi.version, Some((83, 7, 26)));
    }
    // AX210 / AX211: RSA then ECDSA block, command stream at 964.
    for name in [
        "intel/ibt-0041-0041.sfi",
        "intel/ibt-0040-0041.sfi",
        "intel/ibt-0040-1020.sfi",
    ] {
        let Some(data) = firmware(name) else { continue };
        for sbe_type in [0, 1] {
            let sfi = Sfi::parse(data.clone(), Layout::Both { sbe_type }).unwrap();
            assert_eq!(sfi.body_start(), 964);
            assert_eq!(sfi.boot_addr, 0x0010_0800, "{name}");
        }
    }
}

#[test]
fn real_intel_images_download_whole_against_the_scripted_controller() {
    use super::intel::{files, legacy_version, play, tlv_version, FakeIntel};
    use crate::step::{Done, Step};

    // AX210 (TLV, ECDSA engine): headers from 644, commands from 964.
    if let Some(data) = firmware("intel/ibt-0041-0041.sfi") {
        let body_len = data.len() - 964;
        let tlv = tlv_version(0x19, 1, 1, (0x410, 0x410));
        let mut fake = FakeIntel::new(Vec::new(), tlv, body_len);
        let (_, run) = play(
            &mut fake,
            &files(&[("intel/ibt-0041-0041.sfi", data.clone())]),
        );
        assert!(matches!(
            run.last,
            Step::Done(Done::Operational {
                downloaded: true,
                ..
            })
        ));
        assert_eq!(fake.stream(1), data[964..]);
        assert_eq!(fake.stream(0), data[644..772]);
        assert_eq!(run.params(0xfc01)[0][4..], 0x0010_0800u32.to_le_bytes());
    }
    // AX201 (legacy bootloader): commands from 644.
    if let Some(data) = firmware("intel/ibt-19-0-4.sfi") {
        let legacy = legacy_version(0x13, 0x06, (0, 0, 0));
        let mut fake = FakeIntel::new(legacy.clone(), legacy, data.len() - 644);
        let (_, run) = play(&mut fake, &files(&[("intel/ibt-19-0-4.sfi", data.clone())]));
        assert!(matches!(
            run.last,
            Step::Done(Done::Operational {
                downloaded: true,
                ..
            })
        ));
        assert_eq!(fake.stream(1), data[644..]);
        assert_eq!(run.params(0xfc01)[0][4..], 0x0002_4800u32.to_le_bytes());
    }
}

#[test]
fn real_realtek_patches_select_for_their_chip() {
    // RTL8761BU: v1 container, project 14 (0x8761).
    if let Some(fw) = firmware("rtl_bt/rtl8761bu_fw.bin") {
        assert!(epatch::build(&fw, 0x8761, 1, 0).is_ok());
        assert!(epatch::build(&fw, 0x8822, 1, 0).is_err(), "not an RTL8822");
    }
    // RTL8822C: v2 container, project 13.
    if let Some(fw) = firmware("rtl_bt/rtl8822cu_fw.bin") {
        let patch = epatch::build(&fw, 0x8822, 3, 0).unwrap();
        assert!(!patch.is_empty() && patch.len() < fw.len());
        assert!(epatch::build(&fw, 0x8761, 3, 0).is_err());
    }
}
