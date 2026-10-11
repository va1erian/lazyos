//! Station facade tests: port registers, filters, channel bookkeeping,
//! the rate-adaptation inputs, association's H2C sequence, keys, TX
//! packets and C2H rate reports.

use std::vec::Vec;

use super::ra::{get_rate_id, rate_id, update_channel};
use super::*;
use crate::dev::{Band, Bw, Dev, Hal, RF_2T2R};
use crate::fake::FakeHci;
use crate::mac::regs::{HMEBOX, RCR};
use crate::testmodel::{healthy_chip, rfe_efuse};

const MAC: [u8; 6] = [0x02, 0x4c, 0x5a, 0x00, 0x00, 0x01];
const BSSID: [u8; 6] = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60];

fn dev() -> Dev<FakeHci> {
    let mut d = Dev::new(healthy_chip(rfe_efuse(1)));
    crate::bringup::core_init(&mut d);
    crate::mac::set_trx_fifo_info(&mut d.hal.fifo).unwrap();
    d.hal.rf_type = RF_2T2R;
    d.hal.rf_path_num = 2;
    d.efuse.hw_cap.nss = 2;
    d
}

/// The 8-byte H2C commands sent, in order, as (command id, word 0).
fn h2c_cmds(f: &FakeHci) -> Vec<(u8, u32)> {
    f.writes()
        .into_iter()
        .filter(|w| HMEBOX.contains(&w.1))
        .map(|w| (w.2 as u8, w.2))
        .collect()
}

fn ht_peer() -> PeerCaps {
    PeerCaps {
        supp_rates: 0xfff,
        basic_rates: 0x3,
        ht: Some(HtCaps {
            cap: 1 << 5,
            rx_mask: [0xff, 0xff, 0, 0],
            ..HtCaps::default()
        }),
        vht: None,
        bw: Bw::Mhz20,
    }
}

#[test]
fn add_station_programs_port_0() {
    let mut d = dev();
    d.add_station(MAC).unwrap();
    assert_eq!(&d.hci.regs[0x610..0x616], &MAC);
    assert_eq!((d.hci.peek32(0x100) >> 16) & 3, 0, "no link");
    assert_eq!(d.hci.regs[0x550], 0x08, "BIT_EN_BCN_FUNCTION");
    assert!(d.add_station(MAC).is_err());
    d.set_bssid(BSSID);
    assert_eq!(&d.hci.regs[0x618..0x61e], &BSSID);
}

#[test]
fn scan_filter_toggles_bssid_checks() {
    let mut d = dev();
    d.hal.rcr |= crate::mac::regs::RCR_CBSSID_DATA | crate::mac::regs::RCR_CBSSID_BCN;
    d.set_filter_scan(true);
    let rcr = d.hci.peek32(RCR);
    assert_eq!(rcr & ((1 << 7) | (1 << 6)), 0);
    assert_eq!(rcr & (1 << 3), 1 << 3, "broadcast still accepted");
    d.set_filter_scan(false);
    assert_eq!(
        d.hci.peek32(RCR) & (1 << 7),
        1 << 7,
        "only CBSSID_BCN comes back, as upstream"
    );
    d.set_rx_filter(RxFilter {
        control: true,
        other_bss: true,
        ..RxFilter::default()
    });
    assert_eq!(d.hci.peek32(0x6A0) >> 16, 0xffff);
    assert_eq!(d.hci.peek32(RCR) & 1, 1);
}

#[test]
fn channel_bookkeeping_matches_rtw_update_channel() {
    let mut hal = Hal::default();
    assert_eq!(
        update_channel(&mut hal, 42, 36, Bw::Mhz80).unwrap(),
        4,
        "lowest"
    );
    assert_eq!(hal.cch_by_bw, [36, 38, 42]);
    assert_eq!(hal.current_band_type, Band::Ghz5);
    assert_eq!(
        update_channel(&mut hal, 42, 44, Bw::Mhz80).unwrap(),
        1,
        "upper"
    );
    assert_eq!(
        update_channel(&mut hal, 3, 1, Bw::Mhz40).unwrap(),
        2,
        "lower"
    );
    assert_eq!(update_channel(&mut hal, 6, 6, Bw::Mhz20).unwrap(), 0);
    assert_eq!(
        (hal.current_channel, hal.current_band_type),
        (6, Band::Ghz2)
    );
    assert!(update_channel(&mut hal, 15, 15, Bw::Mhz20).is_err());
    assert!(update_channel(&mut hal, 0, 0, Bw::Mhz20).is_err());
    assert!(update_channel(&mut hal, 200, 200, Bw::Mhz20).is_err());
}

#[test]
fn set_channel_defers_calibration_outside_scans() {
    let mut d = dev();
    d.scan_start(MAC).unwrap();
    d.set_channel(1, Bw::Mhz20, 1).unwrap();
    assert!(!d.hal.need_rfk);
    d.scan_complete().unwrap();
    d.set_channel(36, Bw::Mhz20, 36).unwrap();
    assert!(d.hal.need_rfk);
    d.prepare_tx();
    assert!(!d.hal.need_rfk);
}

#[test]
fn rate_inputs_for_an_ht_ap_on_2ghz() {
    let d = dev();
    let si = ra::update_sta_info(&d, 0, &ht_peer());
    assert_eq!(si.ra_mask, 0xFFFF015);
    assert_eq!(si.rate_id, rate_id::BGN_20M_2SS);
    assert!(si.sgi_enable);
    assert_eq!((si.stbc_en, si.ldpc_en, si.init_ra_lv), (0, 0, 1));
}

#[test]
fn rate_inputs_for_a_vht_ap_on_5ghz() {
    let mut d = dev();
    d.hal.current_band_type = Band::Ghz5;
    let peer = PeerCaps {
        supp_rates: 0xff,
        vht: Some(VhtCaps {
            cap: (1 << 4) | (1 << 5) | (1 << 8),
            rx_mcs_map: 0xfffa,
            tx_mcs_map: 0xfffa,
        }),
        ht: Some(HtCaps::default()),
        bw: Bw::Mhz80,
        ..PeerCaps::default()
    };
    let si = ra::update_sta_info(&d, 0, &peer);
    assert_eq!(si.ra_mask, (0x3ff << 12) | (0x3ff << 22) | 0x10);
    assert_eq!(si.rate_id, rate_id::ARFR0_AC_2SS);
    assert!(si.vht_enable && si.sgi_enable);
    assert_eq!((si.stbc_en, si.ldpc_en), (2, 2));
    // One stream only: the 2SS rates go.
    d.efuse.hw_cap.nss = 1;
    let si = ra::update_sta_info(&d, 0, &peer);
    assert_eq!(si.ra_mask, (0x3ff << 12) | 0x10);
    assert_eq!(si.rate_id, rate_id::ARFR1_AC_1SS);
}

#[test]
fn rate_ids_follow_upstream_table() {
    assert_eq!(get_rate_id(1, Bw::Mhz20, 1), rate_id::B_20M);
    assert_eq!(get_rate_id(3, Bw::Mhz20, 1), rate_id::BG);
    assert_eq!(get_rate_id(7, Bw::Mhz40, 2), rate_id::BGN_40M_2SS);
    assert_eq!(
        get_rate_id(1 | 2 | 8, Bw::Mhz40, 1),
        rate_id::ARFR2_AC_2G_1SS
    );
    assert_eq!(get_rate_id(6, Bw::Mhz20, 4), 0, "no 4SS GN id upstream");
    assert_eq!(get_rate_id(0, Bw::Mhz20, 1), 0);
}

#[test]
fn assoc_sends_what_upstream_sends() {
    let mut d = dev();
    d.add_station(MAC).unwrap();
    d.set_bssid(BSSID);
    d.hci.log.clear();
    d.assoc(7, &ht_peer()).unwrap();

    let ids: Vec<u8> = h2c_cmds(&d.hci).iter().map(|c| c.0).collect();
    // default port (0x2c), rsvd page locations (0x00), RA info (0x40),
    // media status (0x01).
    assert_eq!(ids, [0x00, 0x2c, 0x40, 0x01]);
    let rsvd = h2c_cmds(&d.hci)[0].1;
    assert_eq!(
        rsvd >> 8,
        0x03_01_00,
        "probe resp 0, PS-Poll page 1, Null page 3"
    );
    let media = h2c_cmds(&d.hci)[3].1;
    assert_eq!(media, 0x01 | (1 << 8));
    // Two reserved-page writes: the pages, then the dummy first page alone.
    assert_eq!(d.hci.rsvd.len(), 2);
    assert_eq!((d.hci.peek32(0x100) >> 16) & 3, 2, "managed, linked");
    assert_eq!(d.hci.peek32(0x6a8) & 0x7ff, 7);
    assert_eq!(d.hal.vif.sta.unwrap().init_ra_lv, 0);

    d.hci.log.clear();
    d.disassoc().unwrap();
    assert_eq!(
        h2c_cmds(&d.hci),
        [(0x01, 0x01)],
        "media status: disconnected"
    );
    assert_eq!((d.hci.peek32(0x100) >> 16) & 3, 0);
    assert!(d.hal.vif.sta.is_none());
    assert!(
        Dev::new(FakeHci::default()).assoc(1, &ht_peer()).is_err(),
        "no interface"
    );
}

#[test]
fn keys_through_the_facade() {
    let mut d = dev();
    d.sec.enable_sec_engine(&mut d.hci);
    let ptk = [0x11u8; 16];
    let gtk = [0x22u8; 16];
    assert_eq!(d.set_key(Cipher::Ccmp, 0, Some(BSSID), &ptk).unwrap(), 4);
    assert_eq!(d.set_key(Cipher::Ccmp, 1, None, &gtk).unwrap(), 1);
    assert_eq!(d.key_type(4), Some(crate::sec::HwKeyType::Aes));
    assert!(
        d.set_key(Cipher::Ccmp, 4, None, &gtk).is_err(),
        "key index 0-3"
    );
    assert!(
        d.set_key(Cipher::Ccmp, 0, Some(BSSID), &ptk[..8]).is_err(),
        "short key"
    );
    d.del_key(4).unwrap();
    assert_eq!(d.key_type(4), None);
}

fn word(pkt: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(pkt[i * 4..i * 4 + 4].try_into().unwrap())
}

#[test]
fn tx_packets_for_management_and_data() {
    let mut d = dev();
    d.add_station(MAC).unwrap();
    let mut auth = std::vec![0xB0, 0, 0, 0];
    auth.extend_from_slice(&BSSID);
    auth.extend_from_slice(&MAC);
    auth.extend_from_slice(&BSSID);
    auth.extend_from_slice(&[0; 8]);
    let f = d.tx_mgmt(&auth).unwrap();
    assert_eq!(f.queue, crate::hci::TxQueue::Mgmt);
    assert_eq!(word(&f.pkt, 0) & 0xffff, auth.len() as u32);
    assert_eq!((word(&f.pkt, 1) >> 8) & 0x1f, 18);
    assert_eq!(
        word(&f.pkt, 3) & ((1 << 8) | (1 << 10)),
        (1 << 8) | (1 << 10),
        "fixed rate"
    );
    assert_eq!(word(&f.pkt, 4) & 0x7f, 0, "1 Mb/s on 2.4 GHz");
    d.hal.current_band_type = Band::Ghz5;
    assert_eq!(
        word(&d.tx_mgmt(&auth).unwrap().pkt, 4) & 0x7f,
        4,
        "6 Mb/s on 5 GHz"
    );
    d.hal.current_band_type = Band::Ghz2;

    d.set_bssid(BSSID);
    d.assoc(1, &ht_peer()).unwrap();
    let k = d.set_key(Cipher::Ccmp, 0, Some(BSSID), &[1; 16]).unwrap();
    let mut data = std::vec![0x88, 0x41, 0, 0];
    data.extend_from_slice(&BSSID);
    data.extend_from_slice(&MAC);
    data.extend_from_slice(&BSSID);
    data.extend_from_slice(&[0x50, 0x01, 0, 0, 1, 2, 3, 4]);
    let f = d.tx_data(&data, 0, Some(k)).unwrap();
    assert_eq!(f.queue, crate::hci::TxQueue::Be);
    assert_eq!((word(&f.pkt, 1) >> 22) & 3, 3, "CCMP by the chip");
    assert_eq!(
        (word(&f.pkt, 1) >> 16) & 0x1f,
        u32::from(rate_id::BGN_20M_2SS)
    );
    assert_eq!((word(&f.pkt, 9) >> 12) & 0xfff, 0x15, "sequence number");
    assert_eq!(word(&f.pkt, 4) & 0x7f, 0x1b, "starts at MCS15");
    let vo = d.tx_data(&data, 6, None).unwrap();
    assert_eq!(
        (vo.queue, (word(&vo.pkt, 1) >> 8) & 0x1f),
        (crate::hci::TxQueue::Vo, 6)
    );
    let mut bcast = data.clone();
    bcast[4..10].copy_from_slice(&[0xff; 6]);
    assert_eq!(
        d.tx_data(&bcast, 0, None).unwrap().queue,
        crate::hci::TxQueue::Hi0
    );

    assert!(d.tx_mgmt(&data).is_err(), "data is not management");
    assert!(d.tx_data(&auth, 0, None).is_err());
    assert!(d.tx_data(&data, 0, Some(9)).is_err(), "no such key");
    assert!(d.tx_mgmt(&[0xB0, 0]).is_err(), "too short");
    let mut beacon = auth.clone();
    beacon[0] = 0x80;
    assert!(d.tx_mgmt(&beacon).is_err());

    d.send(&f).unwrap();
    assert_eq!((d.hci.tx.len(), d.hci.kicks), (1, 1));
}

#[test]
fn rate_reports_update_the_ap() {
    let mut d = dev();
    d.add_station(MAC).unwrap();
    d.assoc(1, &ht_peer()).unwrap();
    let c2h = d
        .handle_c2h(&[0x0c, 1, 0x80 | 0x13, 0, 0, 0, 0, 0, 1])
        .unwrap();
    assert!(matches!(c2h.event, crate::fw::C2hEvent::RaReport { .. }));
    let si = d.hal.vif.sta.unwrap();
    assert_eq!((si.tx_rate, si.tx_sgi, si.tx_bw), (0x13, true, 1));
    // A report for another MAC id changes nothing.
    d.handle_c2h(&[0x0c, 2, 0x04, 5, 0, 0, 0, 0, 0]).unwrap();
    assert_eq!(d.hal.vif.sta.unwrap().tx_rate, 0x13);
    assert!(d.handle_c2h(&[0x0c]).is_err());
}
