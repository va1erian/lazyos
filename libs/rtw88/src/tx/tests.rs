//! TX descriptor layout against words computed by hand from upstream's
//! field masks, the checksum, and the packet-info builders.

use std::vec::Vec;

use super::*;
use crate::dev::Band;
use crate::hci::TxQueue;

fn words(pkt: &[u8]) -> Vec<u32> {
    pkt[..TX_PKT_DESC_SZ]
        .chunks(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[test]
fn every_field_lands_where_upstream_puts_it() {
    let info = TxPktInfo {
        tx_pkt_size: 0x1234,
        offset: 48,
        bmc: true,
        ls: true,
        dis_qselseq: true,
        mac_id: 5,
        qsel: QSEL_MGMT,
        rate_id: RATEID_B_20M,
        sec_type: SEC_TYPE_CCMP,
        ampdu_en: true,
        report: true,
        ampdu_density: 7,
        bt_null: true,
        hw_ssn_sel: 2,
        use_rate: true,
        dis_rate_fallback: true,
        rts: true,
        nav_use_hdr: true,
        ampdu_factor: 0x1f,
        rate: DESC_RATEMCS7,
        short_gi: true,
        bw: 2,
        ldpc: true,
        stbc: true,
        sn: 0xfc,
        en_hwseq: true,
        seq: 0x123,
        tim_offset: 5,
        ..TxPktInfo::default()
    };
    let mut desc = [0xFFu8; 48];
    fill_tx_desc(&info, &mut desc).unwrap();
    assert_eq!(
        words(&desc),
        std::vec![
            0x8530_1234, // size, offset 48, BMC, LS, DISQSELSEQ
            0x00C8_1205, // mac 5, qsel 18, rate id 8, CCMP
            0x00F8_1000, // AGG_EN, SPE_RPT, density 7, BT_NULL
            0x003E_9580, // ssn sel 2, USE_RATE, DISDATAFB, RTS, NAVUSEHDR, agg 31
            0x0800_0013, // MCS7, RTS at 24M
            0x0000_11D0, // SGI, 80 MHz, LDPC, STBC, RTS short
            0x0000_00FC, // sn
            0,
            0x0000_8000, // EN_HWSEQ
            0x0012_3085, // seq 0x123, TIM_EN, TIM offset 5
            0,
            0,
        ]
    );
    assert!(fill_tx_desc(&info, &mut [0u8; 47]).is_err());
}

#[test]
fn high_queue_sets_more_data_and_excess_bits_drop() {
    let info = TxPktInfo {
        qsel: QSEL_HIGH,
        mac_id: 0xFF,
        rate: 0xFF,
        ..TxPktInfo::default()
    };
    let w = tx_desc_words(&info);
    assert_eq!(w[1], 0xFF | 17 << 8 | 1 << 29);
    assert_eq!(w[4], 0x7F);
}

#[test]
fn checksum_xors_the_descriptor_words() {
    let info = TxPktInfo {
        tx_pkt_size: 2,
        offset: 1,
        ..TxPktInfo::default()
    };
    let mut pkt = build_pkt(&info, &[0x11, 0x22]);
    // w0 = 0x0001_0002 is the only non-zero word: 0x0002 ^ 0x0001.
    fill_txdesc_checksum(&info, &mut pkt).unwrap();
    assert_eq!(words(&pkt)[7], 0x0003);
    // It is recomputed from scratch, not folded into an old value.
    fill_txdesc_checksum(&info, &mut pkt).unwrap();
    assert_eq!(words(&pkt)[7], 0x0003);
    // A pkt_offset covers 8 more bytes per unit, past the descriptor.
    let info = TxPktInfo {
        pkt_offset: 1,
        ..info
    };
    let mut pkt = build_pkt(&info, &[0x11, 0x22, 0, 0, 0, 0, 0, 0]);
    fill_txdesc_checksum(&info, &mut pkt).unwrap();
    let w0 = words(&pkt)[0];
    let w1 = words(&pkt)[1];
    let want = (w0 as u16) ^ (w0 >> 16) as u16 ^ (w1 as u16) ^ (w1 >> 16) as u16 ^ 0x2211;
    assert_eq!(words(&pkt)[7] & 0xFFFF, u32::from(want));
    assert!(fill_txdesc_checksum(&info, &mut pkt[..50]).is_err());
}

#[test]
fn build_pkt_puts_the_payload_behind_the_descriptor() {
    let pkt = build_pkt(&TxPktInfo::default(), &[1, 2, 3]);
    assert_eq!(pkt.len(), 51);
    assert_eq!(words(&pkt)[0], 3);
    assert_eq!(&pkt[48..], &[1, 2, 3]);
}

/// A 24-byte 802.11 header: frame control, `addr1`'s first byte, sequence.
fn hdr(fc: u16, addr1_0: u8, seq: u16) -> Vec<u8> {
    let mut h = std::vec![0u8; 24];
    h[..2].copy_from_slice(&fc.to_le_bytes());
    h[4] = addr1_0;
    h[22..24].copy_from_slice(&(seq << 4).to_le_bytes());
    h
}

#[test]
fn management_frames_go_at_the_basic_rate_on_the_mgmt_queue() {
    let probe = hdr(0x0040, 0xFF, 7);
    let p = TxParams {
        mac_id: 1,
        ..TxParams::default()
    };
    let (q, info) = pkt_info_update(&probe, &p).unwrap();
    assert_eq!(q, TxQueue::Mgmt);
    assert_eq!(
        (info.qsel, info.rate, info.rate_id),
        (QSEL_MGMT, DESC_RATE1M, RATEID_B_20M)
    );
    assert!(info.use_rate && info.dis_rate_fallback && info.dis_qselseq && info.en_hwseq);
    assert!(info.bmc && info.ls);
    assert_eq!(
        (info.offset, info.tx_pkt_size, info.mac_id, info.seq),
        (48, 24, 1, 0)
    );

    let p5 = TxParams {
        band: Band::Ghz5,
        basic_rates: 0b100,
        ..p
    };
    let (_, info) = pkt_info_update(&hdr(0x00b0, 0x02, 0), &p5).unwrap();
    assert_eq!(
        (info.rate, info.rate_id, info.bmc),
        (DESC_RATE12M, RATEID_G, false)
    );
}

#[test]
fn data_frames_take_the_ac_queue_tid_and_station_rate() {
    let sta = StaTx {
        rate: DESC_RATEVHT1SS_MCS9,
        rate_id: RATEID_ARFR1_AC_1SS,
        bw: 2,
        ldpc: true,
        ampdu: Some((ampdu_factor_for_test(3), 5)),
        ..StaTx::default()
    };
    let p = TxParams {
        mac_id: 2,
        ac: Ac::Vi,
        tid: 5,
        sec_type: SEC_TYPE_CCMP,
        sta: Some(sta),
        report_sn: Some(info::report_sn(3)),
        ..TxParams::default()
    };
    let (q, info) = pkt_info_update(&hdr(0x0288, 0x00, 0x123), &p).unwrap();
    assert_eq!(q, TxQueue::Vi);
    assert_eq!(
        (info.qsel, info.rate, info.rate_id, info.bw),
        (5, 0x35, 10, 2)
    );
    assert_eq!((info.seq, info.sec_type, info.sn), (0x123, 3, 12));
    assert!(info.ldpc && info.ampdu_en && info.report && !info.use_rate && !info.bmc);
    assert_eq!((info.ampdu_factor, info.ampdu_density), (31, 5));

    // Group data: the high queue, 6 Mb/s on the BG table, MORE_DATA.
    let (q, info) = pkt_info_update(&hdr(0x0208, 0x01, 1), &TxParams::default()).unwrap();
    assert_eq!(q, TxQueue::Hi0);
    assert_eq!(
        (info.qsel, info.rate, info.rate_id),
        (QSEL_HIGH, DESC_RATE6M, RATEID_BG)
    );
    assert_ne!(tx_desc_words(&info)[1] & 1 << 29, 0);

    // A fixed rate wins and turns fallback off.
    let p = TxParams {
        fix_rate: Some(DESC_RATE54M),
        ..TxParams::default()
    };
    let (_, info) = pkt_info_update(&hdr(0x0208, 0x00, 1), &p).unwrap();
    assert_eq!(info.rate, DESC_RATE54M);
    assert!(info.use_rate && info.dis_rate_fallback);
}

fn ampdu_factor_for_test(exp: u8) -> u8 {
    info::ampdu_factor(exp)
}

#[test]
fn null_frames_are_management_like_but_queue_by_ac() {
    let p = TxParams {
        ac: Ac::Vo,
        tid: 6,
        ..TxParams::default()
    };
    let (q, info) = pkt_info_update(&hdr(0x0248, 0x00, 0), &p).unwrap();
    assert_eq!((q, info.qsel), (TxQueue::Vo, 6));
    assert!(info.use_rate && info.en_hwseq);
    // QoS Null too.
    let (_, info) = pkt_info_update(&hdr(0x02c8, 0x00, 0), &p).unwrap();
    assert!(info.en_hwseq);
}

#[test]
fn beacons_control_frames_and_short_frames() {
    let (q, info) = pkt_info_update(&hdr(0x0080, 0xFF, 0), &TxParams::default()).unwrap();
    assert_eq!((q, info.qsel), (TxQueue::Bcn, QSEL_BEACON));
    // PS-Poll is 16 bytes: a control frame, no rate update.
    let pspoll = &hdr(0x00a4, 0x00, 0)[..16];
    let (q, info) = pkt_info_update(pspoll, &TxParams::default()).unwrap();
    assert_eq!(
        (q, info.qsel, info.use_rate),
        (TxQueue::Mgmt, QSEL_MGMT, false)
    );
    assert!(pkt_info_update(&[0u8; 9], &TxParams::default()).is_err());
    // A data frame needs its sequence control.
    assert!(pkt_info_update(&hdr(0x0208, 0, 0)[..20], &TxParams::default()).is_err());
}

#[test]
fn reserved_page_and_h2c_packets() {
    let chunk = [0xA5u8; 64];
    let pkt = rsvd_page_pkt(&chunk, Band::Ghz2);
    let w = words(&pkt);
    // size 64, offset 48, BMC (0xA5 has the group bit), LS, DISQSELSEQ.
    assert_eq!(w[0], 64 | 48 << 16 | 1 << 24 | 1 << 26 | 1 << 31);
    assert_eq!(
        w[1],
        u32::from(QSEL_BEACON) << 8 | u32::from(RATEID_B_20M) << 16
    );
    assert_eq!(w[3], 1 << 8 | 1 << 10);
    assert_eq!(w[8], 1 << 15);
    assert_eq!(&pkt[48..], &chunk);

    let ps = rsvd_page_pkt_info(&hdr(0x00a4, 0, 0), RsvdPacket::PsPoll, Band::Ghz5);
    assert!(ps.nav_use_hdr && !ps.en_hwseq);
    assert_eq!((ps.qsel, ps.rate), (QSEL_MGMT, DESC_RATE6M));
    let qn = rsvd_page_pkt_info(&hdr(0x02c8, 0, 0), RsvdPacket::QosNull, Band::Ghz2);
    assert!(qn.bt_null && qn.en_hwseq);

    let h2c = h2c_pkt(&[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(words(&h2c)[..2], [8, u32::from(QSEL_H2C) << 8]);
    assert!(words(&h2c)[2..].iter().all(|w| *w == 0));
}

#[test]
fn queue_helpers() {
    assert_eq!(
        [0, 1, 2, 3, 4, 5, 6, 7].map(Ac::from_tid),
        [
            Ac::Be,
            Ac::Bk,
            Ac::Bk,
            Ac::Be,
            Ac::Vi,
            Ac::Vi,
            Ac::Vo,
            Ac::Vo
        ]
    );
    assert_eq!(
        [Ac::Vo, Ac::Vi, Ac::Be, Ac::Bk].map(ac_to_hwq),
        [TxQueue::Vo, TxQueue::Vi, TxQueue::Be, TxQueue::Bk]
    );
    assert_eq!(pci_qsel(TxQueue::H2c, 3), QSEL_H2C);
    assert_eq!(pci_qsel(TxQueue::Bk, 1), 1);
    assert_eq!(info::highest_ht_tx_rate(true, 0xff), DESC_RATEMCS15);
    assert_eq!(info::highest_ht_tx_rate(true, 0), DESC_RATEMCS7);
    assert_eq!(info::highest_vht_tx_rate(2, 0b0110), DESC_RATEVHT2SS_MCS8);
    assert_eq!(info::highest_vht_tx_rate(1, 0b11), DESC_RATEVHT1SS_MCS9);
    assert_eq!(info::legacy_tx_rate(0xf), DESC_RATE11M);
    assert_eq!(info::legacy_tx_rate(0xff0), DESC_RATE54M);
    assert_eq!(info::ampdu_factor(0), 3);
}
