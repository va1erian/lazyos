//! RX descriptor parsing against hand-made words, and the seeded fuzz run.

use std::vec::Vec;

use super::fuzz::{encode_rx_desc, RxDescFields};
use super::*;

/// The descriptor as raw words, then `rest`.
fn raw(words: [u32; 6], rest: &[u8]) -> Vec<u8> {
    let mut b: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    b.extend_from_slice(rest);
    b
}

#[test]
fn a_data_frame_with_phy_status() {
    // pkt_len 100, drv_info 4 units (32 bytes), shift 2, PHYST, AES, HW
    // decrypted; cam 5; ppdu 2; rate MCS7; 40 MHz; TSF 0xdeadbeef.
    let w0 = 100 | 4 << 16 | 4 << 20 | 2 << 24 | 1 << 26;
    let w = [w0, 5, 2 << 29, 0x13, 1 << 4, 0xdead_beef];
    let mut rest = std::vec![0xAA; 2];
    rest.extend([0xBB; 32]);
    rest.extend([0xCC; 100]);
    let buf = raw(w, &rest);
    let mut stat = RxPktStat::default();
    let mut phy = Vec::new();
    let layout = query_rx_desc_with(&buf, &mut stat, |p, _| phy = p.to_vec()).unwrap();
    assert_eq!(
        layout,
        RxLayout {
            offset: 24 + 2 + 32,
            len: 100,
            is_c2h: false
        }
    );
    assert_eq!(phy, std::vec![0xBB; 32]);
    assert!(layout.payload(&buf).iter().all(|b| *b == 0xCC));
    assert!(stat.phy_status && stat.decrypted && !stat.is_c2h);
    assert!(!stat.crc_err && !stat.icv_err);
    assert_eq!(
        (stat.pkt_len, stat.drv_info_sz, stat.shift, stat.cam_id),
        (100, 32, 2, 5)
    );
    assert_eq!(
        (stat.ppdu_cnt, stat.rate, stat.bw, stat.tsf_low),
        (2, 0x13, 1, 0xdead_beef)
    );
    assert_eq!(rx_desc_extent(&buf), Ok(buf.len()));
}

#[test]
fn decrypted_needs_an_encryption_type_and_no_swdec() {
    let cases = [
        (RX_DESC_ENC_NONE, false, false),
        (RX_DESC_ENC_AES, false, true),
        (RX_DESC_ENC_AES, true, false),
        (RX_DESC_ENC_TKIP_MIC, false, true),
    ];
    for (enc, swdec, want) in cases {
        let f = RxDescFields {
            pkt_len: 30,
            enc_type: enc as u8,
            swdec,
            crc_err: true,
            icv_err: true,
            ..RxDescFields::default()
        };
        let mut buf = encode_rx_desc(&f).to_vec();
        buf.extend([0; 30]);
        let mut stat = RxPktStat::default();
        query_rx_desc(&buf, &mut stat).unwrap();
        assert_eq!(stat.decrypted, want, "enc {enc} swdec {swdec}");
        assert!(stat.crc_err && stat.icv_err);
    }
}

#[test]
fn c2h_skips_the_frame_checks_and_the_phy_status() {
    // A C2H of 2 bytes would be "shorter than its FCS" as a frame.
    let f = RxDescFields {
        pkt_len: 2,
        c2h: true,
        drv_info_units: 4,
        phy_status: true,
        ..RxDescFields::default()
    };
    let mut buf = encode_rx_desc(&f).to_vec();
    buf.extend([0; 32]);
    buf.extend([0x0A, 0x0B]);
    let mut stat = RxPktStat::default();
    let layout =
        query_rx_desc_with(&buf, &mut stat, |_, _| panic!("no phy status for C2H")).unwrap();
    assert!(layout.is_c2h && stat.is_c2h);
    assert_eq!(layout.offset, 24 + 32);
    assert_eq!(layout.payload(&buf), &[0x0A, 0x0B]);
}

#[test]
fn refusals_follow_upstream() {
    let ok = RxDescFields {
        pkt_len: 30,
        ..RxDescFields::default()
    };
    let cases = [
        RxDescFields { rate: 0x54, ..ok },
        RxDescFields {
            drv_info_units: 2,
            ..ok
        },
        RxDescFields {
            phy_status: true,
            ..ok
        },
        RxDescFields {
            pkt_len: 11455,
            ..ok
        },
        RxDescFields { pkt_len: 4, ..ok },
    ];
    for f in cases {
        let mut buf = encode_rx_desc(&f).to_vec();
        buf.extend(std::vec![0; 12_000]);
        let mut stat = RxPktStat::default();
        assert!(query_rx_desc(&buf, &mut stat).is_err(), "{f:?}");
    }
    // The largest legal values pass.
    let f = RxDescFields {
        pkt_len: 11454,
        rate: 0x53,
        ..ok
    };
    let mut buf = encode_rx_desc(&f).to_vec();
    buf.extend(std::vec![0; 11454]);
    assert!(query_rx_desc(&buf, &mut RxPktStat::default()).is_ok());
}

#[test]
fn truncated_buffers_are_refused_not_sliced() {
    let mut stat = RxPktStat::default();
    assert!(query_rx_desc(&[0; 23], &mut stat).is_err());
    assert!(rx_desc_extent(&[0; 23]).is_err());
    let f = RxDescFields {
        pkt_len: 30,
        drv_info_units: 4,
        phy_status: true,
        shift: 3,
        ..RxDescFields::default()
    };
    let mut buf = encode_rx_desc(&f).to_vec();
    buf.extend([0; 3 + 32 + 29]);
    assert!(query_rx_desc(&buf, &mut stat).is_err());
    buf.push(0);
    assert!(query_rx_desc(&buf, &mut stat).is_ok());
    // A refused buffer leaves no stale fields behind on the next one.
    let mut other = RxPktStat {
        rssi: 9,
        ..RxPktStat::default()
    };
    let _ = query_rx_desc(&[0; 24], &mut other);
    assert_eq!(other.rssi, 0);
}

#[test]
fn seeded_rx_fuzz() {
    fuzzkit::for_seeds("rtw88::rx::seeded_rx_fuzz", |_, rng| {
        let len = rng.below(1500) as usize;
        let mut data = rng.bytes(len);
        // Half the time start from a plausible descriptor.
        if rng.one_in(2) && data.len() >= 24 {
            let f = RxDescFields {
                pkt_len: rng.below(400) as u16,
                drv_info_units: if rng.one_in(2) { 4 } else { 0 },
                shift: rng.below(4) as u8,
                phy_status: rng.one_in(2),
                c2h: rng.one_in(4),
                rate: rng.below(0x58) as u8,
                ..RxDescFields::default()
            };
            data[..24].copy_from_slice(&encode_rx_desc(&f));
        }
        fuzz::run(&data);
    });
}
