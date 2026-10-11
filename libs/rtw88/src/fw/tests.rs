//! Firmware tests: header parsing (synthetic images, hostile sizes, the
//! real file when `RTW88_FW` names it), the H2C mailboxes and packets, the
//! command layouts, C2H decoding and the reserved-page path.

use std::vec::Vec;

use super::*;
use crate::dev::Dev;
use crate::error::Error;
use crate::fake::FakeHci;
use crate::mac::regs::{BCN_CTRL, BCN_VALID_V1, FIFOPAGE_CTRL_2, HMEBOX, HMEBOX_EX, HMETFR};
use crate::testmodel::{real_fw, synth_fw};
use fuzzkit::Rng;

#[test]
fn parses_a_synthetic_image() {
    let img = synth_fw(0x4b80, 0xd330, Some(0x1f860));
    let fw = Firmware::parse(&img).unwrap();
    assert_eq!((fw.hdr.version, fw.hdr.subversion, fw.hdr.subindex), (9, 9, 15));
    assert_eq!(fw.dmem.addr, 0x0020_0000);
    assert_eq!(fw.dmem.data.len(), 0x4b88);
    assert_eq!(fw.imem.addr, 0x0003_0000);
    assert_eq!(fw.imem.data.len(), 0xd338);
    assert_eq!(fw.emem.unwrap().addr, 0x0010_0000);
    assert_eq!(fw.feature(), 0x1e7);
    assert_eq!(fw.feature_ext(), 0);

    let mut st = FwState::default();
    st.update(&fw);
    assert!(st.has(feature::BCN_FILTER));
    assert_eq!(st.h2c_version, 0x0f);
}

#[test]
fn rejects_images_whose_sizes_do_not_add_up() {
    let img = synth_fw(0x100, 0x100, None);
    assert!(Firmware::parse(&img).is_ok());
    assert_eq!(Firmware::parse(&img[..img.len() - 1]).unwrap_err(), Error::BadFirmware("section sizes do not match the file"));
    assert!(Firmware::parse(&img[..63]).is_err());
    assert!(Firmware::parse(&[]).is_err());

    // Sizes near u32::MAX must not wrap into a match.
    let mut bad = img.clone();
    bad[0x24..0x28].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Firmware::parse(&bad).is_err());
    // EMEM claimed but absent.
    let mut bad = img.clone();
    bad[0x18] |= 0x10;
    bad[0x34..0x38].copy_from_slice(&4u32.to_le_bytes());
    assert!(Firmware::parse(&bad).is_err());
}

#[test]
fn unsigned_and_old_firmware_flags() {
    let mut img = synth_fw(0x10, 0x10, None);
    img[0x0C] = 0xe6; // SIG bit clear
    img[7] = 12; // 9.9.12
    let fw = Firmware::parse(&img).unwrap();
    assert_eq!(fw.feature(), 0);
    assert_eq!(fw.feature_ext(), feature::EXT_OLD_PAGE_NUM);
}

#[test]
fn legacy_header() {
    let mut h = [0u8; 32];
    h[4] = 0x24;
    h[6] = 1;
    h[7] = 2;
    h[0x0C] = 0x34;
    h[0x0D] = 0x12;
    let l = FwHeaderLegacy::parse(&h).unwrap();
    assert_eq!((l.version, l.subversion1, l.subversion2, l.size), (0x24, 1, 2, 0x1234));
    assert!(FwHeaderLegacy::parse(&h[..31]).is_err());
}

#[test]
fn real_firmware_parses_when_given() {
    let Some(img) = real_fw() else {
        return;
    };
    let fw = Firmware::parse(&img).unwrap();
    assert_eq!(fw.hdr.version, 9);
    assert!(fw.emem.is_some(), "the 8822C image carries EMEM");
    assert_eq!(fw.dmem.addr, 0x0020_0000);
    assert_eq!(fw.imem.addr, 0x0003_0000);
    assert!(fw.feature() & feature::SIG != 0);
}

#[test]
fn fuzzed_images_and_c2h_never_panic() {
    let mut rng = Rng::new(0x8822c);
    let base = synth_fw(0x40, 0x40, Some(0x20));
    for _ in 0..20_000 {
        let n = rng.below(300) as usize;
        let mut img = if rng.one_in(2) { base.clone() } else { rng.bytes(n) };
        for _ in 0..rng.below(4) {
            if !img.is_empty() {
                let at = rng.below(img.len() as u64) as usize;
                img[at] = rng.byte();
            }
        }
        if let Ok(fw) = Firmware::parse(&img) {
            let total = 64 + fw.dmem.data.len() + fw.imem.data.len() + fw.emem.map_or(0, |e| e.data.len());
            assert_eq!(total, img.len());
        }
        let _ = parse_c2h(&img[..img.len().min(rng.below(20) as usize)]);
    }
}

fn box_writes(f: &FakeHci) -> Vec<(u32, u32)> {
    f.writes().into_iter().map(|w| (w.1, w.2)).collect()
}

#[test]
fn h2c_boxes_rotate_ext_first() {
    let mut d = Dev::new(FakeHci::default());
    for i in 0..5u8 {
        send_h2c_cmd(&mut d, &[i, 0xA, 0xB, 0xC, 1, 2, 3, i]).unwrap();
    }
    let w = box_writes(&d.hci);
    assert_eq!(w.len(), 10);
    for i in 0..5usize {
        let b = i % 4;
        assert_eq!(w[2 * i], (HMEBOX_EX[b], 0x0003_0201 | ((i as u32) << 24)));
        assert_eq!(w[2 * i + 1], (HMEBOX[b], 0x0C0B_0A00 | i as u32));
    }
    assert_eq!(d.fw.last_box_num, 1);
}

#[test]
fn h2c_waits_for_a_busy_box() {
    let mut d = Dev::new(FakeHci::default());
    d.fw.last_box_num = 2;
    d.hci.force.push((HMETFR, 1 << 2, 1 << 2));
    assert!(matches!(send_h2c_cmd(&mut d, &[0; 8]), Err(Error::Timeout { step: "h2c mailbox", .. })));
    assert_eq!(d.fw.last_box_num, 2, "a failed command does not use up the box");
    assert!(d.hci.writes().is_empty());
}

#[test]
fn h2c_packets_carry_sequence_and_descriptor() {
    let mut d = Dev::new(FakeHci::default());
    crate::mac::set_trx_fifo_info(&mut d.hal.fifo).unwrap();
    send_general_info(&mut d).unwrap();
    d.hal.rf_type = crate::dev::RF_2T2R;
    d.hal.cut_version = 1;
    d.hal.antenna_tx = 3;
    d.hal.antenna_rx = 3;
    d.efuse.rfe_option = 5;
    send_phydm_info(&mut d).unwrap();
    do_iqk(&mut d, &IqkPara { clear: 1, segment_iqk: 0 }).unwrap();

    assert_eq!(d.hci.h2c.len(), 3);
    for (seq, p) in d.hci.h2c.iter().enumerate() {
        assert_eq!(p.len(), 48 + 32);
        let w0 = u32::from_le_bytes(p[0..4].try_into().unwrap());
        let w1 = u32::from_le_bytes(p[4..8].try_into().unwrap());
        assert_eq!(w0, 32, "size only, offset 0 (rtw_tx_write_data_h2c_get)");
        assert_eq!(w1, 19 << 8, "QSEL_H2C");
        let pkt = &p[48..];
        assert_eq!(u16::from_le_bytes([pkt[0], pkt[1]]), 0xFF01);
        assert_eq!(u16::from_le_bytes([pkt[6], pkt[7]]) as usize, seq);
    }
    let gi = &d.hci.h2c[0][48..];
    assert_eq!((gi[2], gi[4]), (0x0D, 12));
    assert_eq!(gi[10], (1994 - 1938) as u8, "fw tx boundary");
    let pi = &d.hci.h2c[1][48..];
    assert_eq!((pi[2], pi[4]), (0x11, 16));
    assert_eq!(&pi[8..12], &[5, 2, 1, 0x33]);
    let iqk = &d.hci.h2c[2][48..];
    assert_eq!((iqk[2], iqk[4], iqk[8]), (0x0E, 9, 1));
    assert_eq!(d.fw.seq, 3);
}

#[test]
fn command_layouts() {
    let mut d = Dev::new(FakeHci::default());
    let ra = RaInfo {
        mac_id: 0,
        rate_id: 9,
        init_ra_lv: 1,
        sgi_enable: true,
        bw_mode: 2,
        ldpc_en: 2,
        vht_enable: true,
        ra_mask: 0x1_2345_6789,
    };
    send_ra_info(&mut d, &ra, true).unwrap();
    media_status_report(&mut d, 3, true).unwrap();
    set_pwr_mode(&mut d, &PwrMode::ACTIVE).unwrap();
    default_port(&mut d, 0, 0).unwrap();
    let w = box_writes(&d.hci);
    assert_eq!(w[0], (HMEBOX_EX[0], 0x2345_6789), "low 32 bits of the mask");
    assert_eq!(w[1].1, 0x40 | (9 << 16) | (1 << 21) | (1 << 23) | (2 << 24) | (1 << 26) | (1 << 28) | (1 << 30));
    assert_eq!(w[3].1, 0x01 | (1 << 8) | (3 << 16));
    assert_eq!((w[4].1, w[5].1), (0xC << 8, 0x20 | (1 << 24)));
    assert_eq!(w[7].1, 0x2c);
}

#[test]
fn c2h_events_decode_and_short_ones_are_refused() {
    let ra = parse_c2h(&[0x0c, 7, 0x80 | 0x35, 0, 0, 0, 0, 0, 2]).unwrap();
    assert_eq!(ra.event, C2hEvent::RaReport { mac_id: 0, rate: 0x35, sgi: true, bw: Some(2) });
    assert!(parse_c2h(&[0x0c, 7, 0x35, 0]).is_err());

    let ccx = parse_c2h(&[0x03, 0, 0x00, 0, 0, 0, 0, 0, 0x14]).unwrap();
    assert_eq!(ccx.event, C2hEvent::TxReport { sn: 0x14, acked: true });
    let ext = parse_c2h(&[0xff, 1, 0x0f, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x40]).unwrap();
    assert_eq!(ext.event, C2hEvent::TxReport { sn: 0x08, acked: false });
    let sw = parse_c2h(&[0xff, 1, 0x22, 0, 36, 1, 0]).unwrap();
    assert_eq!(sw.event, C2hEvent::ChanSwitch { central_ch: 36, id: 1, status: 0 });
    assert!(parse_c2h(&[0xff, 1]).is_err());
    assert!(parse_c2h(&[0xff, 1, 0x22, 0]).is_err());
    let bcn = parse_c2h(&[0x36, 0, 0, 0x11, 40]).unwrap();
    assert_eq!(bcn.event, C2hEvent::BcnFilter { kind: 1, high: true, rssi: -60 });
    assert!(matches!(parse_c2h(&[0x09, 0, 1, 2]).unwrap().event, C2hEvent::Coex { id: 0x09, .. }));
    assert!(matches!(parse_c2h(&[0x77, 0]).unwrap().event, C2hEvent::Other { id: 0x77, .. }));
    assert!(parse_c2h(&[0x0c]).is_err());
}

#[test]
fn rsvd_page_write_restores_the_beacon_registers() {
    let mut d = Dev::new(FakeHci::default());
    crate::mac::set_trx_fifo_info(&mut d.hal.fifo).unwrap();
    d.hci.regs[BCN_CTRL as usize] = 0x18;
    rsvd::write_data_rsvd_page(&mut d, 0x7a2, &[1, 2, 3]).unwrap();
    let w = d.hci.writes();
    assert_eq!(w[0], (2, FIFOPAGE_CTRL_2, u32::from(0x7a2 | BCN_VALID_V1)));
    assert_eq!(d.hci.rsvd.len(), 1);
    assert_eq!(&d.hci.rsvd[0][48..], &[1, 2, 3]);
    // Restored: the beacon head back at the boundary, BCN_CTRL as before.
    assert_eq!(d.hci.peek32(FIFOPAGE_CTRL_2) & 0xffff, 1938 | u32::from(BCN_VALID_V1));
    assert_eq!(d.hci.regs[BCN_CTRL as usize], 0x18);
    assert!(rsvd::write_data_rsvd_page(&mut d, 0, &[]).is_err());
}

#[test]
fn station_reserved_pages_are_page_aligned() {
    let mac = [2, 0, 0, 0, 0, 1];
    let bssid = [0x10, 0, 0, 0, 0, 2];
    let (buf, loc) = rsvd::build_sta_rsvd_page(&mac, &bssid, 5, crate::dev::Band::Ghz2);
    assert_eq!(loc, rsvd::RsvdLocations { ps_poll: 1, qos_null: 2, null: 3 });
    assert_eq!(buf.len(), 3 * 128 + 80);
    // Page p >= 1 starts at 80 + 128 * (p - 1): its descriptor, then the frame.
    let ps = &buf[80..80 + 48 + 16];
    assert_eq!(&ps[48..52], &[0xA4, 0x00, 5, 0xC0]);
    assert_eq!(&ps[52..58], &bssid);
    let w1 = u32::from_le_bytes(ps[4..8].try_into().unwrap());
    assert_eq!((w1 >> 8) & 0x1f, 18, "QSEL_MGMT");
    let w3 = u32::from_le_bytes(ps[12..16].try_into().unwrap());
    assert!(w3 & (1 << 15) != 0, "PS-Poll uses the NAV from its header");
    let qos = &buf[80 + 128..80 + 128 + 48 + 26];
    assert_eq!(&qos[48..50], &[0xC8, 0x01]);
    let w2 = u32::from_le_bytes(qos[8..12].try_into().unwrap());
    assert!(w2 & (1 << 23) != 0, "QoS Null is the BT null");
    assert_eq!(&buf[80 + 256 + 48..80 + 256 + 50], &[0x48, 0x01]);
}
