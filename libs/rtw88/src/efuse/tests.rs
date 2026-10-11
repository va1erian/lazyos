//! Efuse tests: the logical map from synthetic physical logs (both header
//! forms, skipped words, malformed records), the physical read through
//! `REG_EFUSE_CTRL`, and the hardware feature report.

use std::vec;
use std::vec::Vec;

use super::*;
use crate::dev::Dev;
use crate::error::Error;
use crate::fake::FakeHci;
use crate::testmodel::{healthy_chip, HW_FEATURE_W1};
use fuzzkit::Rng;

/// A 1-byte header record: block `blk` (< 16), words where `word_en` has a 0.
fn rec1(blk: u8, word_en: u8, data: &[u8]) -> Vec<u8> {
    let mut v = vec![(blk << 4) | word_en];
    v.extend_from_slice(data);
    v
}

/// A 2-byte header record for any block below 128.
fn rec2(blk: u8, word_en: u8, data: &[u8]) -> Vec<u8> {
    let mut v = vec![((blk & 7) << 5) | 0x0f, ((blk >> 3) << 4) | word_en];
    v.extend_from_slice(data);
    v
}

fn phys(records: &[Vec<u8>]) -> Vec<u8> {
    let mut p: Vec<u8> = records.concat();
    p.resize(512, 0xff);
    p
}

#[test]
fn logical_map_replays_both_header_forms() {
    let p = phys(&[
        // Block 1, words 0 and 2 (word_en 0b1010): logical 8..10 and 12..14.
        rec1(1, 0b1010, &[0x11, 0x12, 0x13, 0x14]),
        // Block 0x20 (logical 0x100), word 3 only.
        rec2(0x20, 0b0111, &[0xAA, 0xBB]),
        // A later write to block 1 word 0 wins.
        rec1(1, 0b1110, &[0x21, 0x22]),
    ]);
    let log = logical_map(&p, 512, 124, 768).unwrap();
    assert_eq!(log.len(), 768);
    assert_eq!(&log[8..14], &[0x21, 0x22, 0xff, 0xff, 0x13, 0x14]);
    assert_eq!(&log[0x106..0x108], &[0xAA, 0xBB]);
    assert!(log[..8].iter().all(|&b| b == 0xff));
    assert_eq!(log.iter().filter(|&&b| b != 0xff).count(), 6);
}

#[test]
fn logical_map_stops_at_the_end_marker() {
    let mut p = phys(&[rec1(0, 0b1110, &[1, 2])]);
    // 2-byte header with an erased second byte ends the log too.
    p[3] = 0x0f;
    p[4] = 0xff;
    p[5] = 0x00;
    let log = logical_map(&p, 512, 124, 768).unwrap();
    assert_eq!(&log[0..2], &[1, 2]);
    assert!(log[2..].iter().all(|&b| b == 0xff));
}

#[test]
fn malformed_logs_are_errors() {
    // Block 127 is past a 768-byte logical map.
    let p = phys(&[rec2(127, 0b1110, &[1, 2])]);
    assert_eq!(logical_map(&p, 512, 124, 768), Err(Error::Efuse("efuse record past the end of a map")));
    // A record running past the readable part (512 - 124 bytes).
    let mut p = vec![0u8; 512];
    p[386] = 0x00; // block 0, all four words, at the very end
    for b in p.iter_mut().take(386) {
        *b = 0x0f; // 2-byte headers of block 0 with word_en 0xf: nothing written
    }
    assert!(logical_map(&p, 512, 124, 768).is_err());
    // Sizes larger than the buffer are clamped, not trusted.
    assert!(logical_map(&[0x0e, 1, 2], 4096, 0, 768).is_ok());
}

#[test]
fn fuzzed_physical_maps_never_panic() {
    let mut rng = Rng::new(0xefe);
    for _ in 0..5_000 {
        let len = rng.below(600) as usize;
        let p = rng.bytes(len);
        let phy = rng.below(700) as u32;
        let prot = rng.below(200) as u32;
        let logical = rng.below(900) as u32;
        if let Ok(log) = logical_map(&p, phy, prot, logical) {
            assert_eq!(log.len(), logical as usize);
        }
    }
}

#[test]
fn physical_read_goes_through_efuse_ctrl() {
    let p = phys(&[rec1(0, 0b1100, &[0x02, 0x4c, 0x5a, 0x88])]);
    let mut d = Dev::new(healthy_chip(p.clone()));
    d.efuse.physical_size = 512;
    d.efuse.protect_size = 124;
    d.efuse.logical_size = 768;
    d.hci.regs[0x29] = 0x01; // LDO25 on
    parse_efuse_map(&mut d).unwrap();
    assert_eq!(d.hci.regs[0x29] & 1, 0, "2.5V LDO off for reading");
    assert_eq!(d.hci.peek32(crate::mac::regs::LDO_EFUSE_CTRL) & 0x300, 0, "Wi-Fi bank");
    // Every byte asked for with the flag clear, at its address.
    let reads: Vec<u32> = d
        .hci
        .writes()
        .into_iter()
        .filter(|w| w.1 == 0x30)
        .map(|w| (w.2 >> 8) & 0x3ff)
        .collect();
    assert_eq!(reads, (0..512).collect::<Vec<u32>>());

    let mut f = healthy_chip(p);
    assert_eq!(read8_physical_efuse(&mut f, 2).unwrap(), 0x4c);
}

#[test]
fn physical_read_times_out_without_the_flag() {
    let mut d = Dev::new(FakeHci::default());
    d.efuse.physical_size = 1;
    assert!(matches!(parse_efuse_map(&mut d), Err(Error::Timeout { step: "efuse read", .. })));
}

#[test]
fn hw_feature_report() {
    let mut d = Dev::new(healthy_chip(vec![]));
    d.hal.rf_path_num = 2;
    read::request_hw_feature(&mut d.hci);
    dump_hw_feature(&mut d).unwrap();
    assert_eq!(d.efuse.hw_cap, HwCap { hci: 0, bw: 0b111, ptcl: 3, nss: 2, ant_num: 2 });
    assert_eq!(d.hci.regs[0x1A0], 0, "report consumed");
    assert!(dump_hw_feature(&mut d).is_err(), "no second report");

    // One antenna on a 2T2R chip: the RF configuration drops to 1T1R.
    let mut d = Dev::new(FakeHci::default());
    d.hal.rf_path_num = 2;
    d.hci.regs[0x1A0] = 0x19;
    let w1 = (HW_FEATURE_W1 & !(7 << 21)) | (1 << 21);
    d.hci.regs[0x1A6..0x1AA].copy_from_slice(&w1.to_le_bytes());
    dump_hw_feature(&mut d).unwrap();
    assert_eq!((d.hal.rf_path_num, d.hal.rf_type, d.efuse.hw_cap.nss), (1, crate::dev::RF_1T1R, 1));
}

#[test]
fn defaults_replace_erased_fields() {
    let mut e = Efuse {
        crystal_cap: 0xff,
        channel_plan: 0xff,
        rf_board_option: 0x20,
        pa_type_2g: 0x10,
        lna_type_5g: 0x08,
        bt_setting: 1,
        addr: [0x02, 0, 0, 0, 0, 1],
        ..Efuse::default()
    };
    assert!(e.apply_defaults());
    assert_eq!((e.crystal_cap, e.channel_plan), (0, 0x7f));
    assert!(e.btcoex && e.share_ant && e.ext_pa_2g && e.ext_lna_5g && !e.ext_pa_5g);
    e.addr = [0xff; 6];
    assert!(!e.apply_defaults());
    e.addr = [0; 6];
    assert!(!e.apply_defaults());
}
