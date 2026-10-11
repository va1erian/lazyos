//! Data frames, CCMP framing, the replay window and frame classes.

use std::vec;
use std::vec::Vec;

use super::{AP, STA};
use crate::ccmp::{self, Replay};
use crate::class::{classify, classify_msdu, FrameClass, MsduClass};
use crate::data::*;
use crate::Error;

const GW: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];

fn ethernet(dst: &[u8; 6], src: &[u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut eth = Vec::new();
    eth.extend_from_slice(dst);
    eth.extend_from_slice(src);
    eth.extend_from_slice(&ethertype.to_be_bytes());
    eth.extend_from_slice(payload);
    eth
}

fn to_ds(tid: Option<u8>, security: Option<[u8; 8]>) -> Encap {
    Encap {
        direction: Direction::ToDs,
        bssid: AP,
        tid,
        sequence: 0x123,
        security,
    }
}

#[test]
fn qos_to_ds_frame_layout() {
    let eth = ethernet(&GW, &STA, 0x0800, &[0x45, 0xB8, 1, 2, 3]);
    let frame = encap(&eth, &to_ds(Some(5), None)).unwrap();
    assert_eq!(frame[0], 0x88, "QoS data");
    assert_eq!(frame[1], fc::TO_DS);
    assert_eq!(&frame[4..10], &AP);
    assert_eq!(&frame[10..16], &STA);
    assert_eq!(&frame[16..22], &GW);
    assert_eq!(u16::from_le_bytes([frame[22], frame[23]]), 0x123 << 4);
    assert_eq!(&frame[24..26], &[5, 0]);
    assert_eq!(&frame[26..32], &RFC1042);
    assert_eq!(&frame[32..34], &[0x08, 0x00]);
    assert_eq!(&frame[34..], &[0x45, 0xB8, 1, 2, 3]);
    let h = DataHeader::parse(&frame).unwrap();
    assert_eq!((h.len, h.tid(), h.bssid()), (26, Some(5), Some(AP)));
    assert_eq!((h.source(), h.destination()), (STA, GW));
}

#[test]
fn plain_data_and_bridge_tunnel() {
    let eth = ethernet(&GW, &STA, ETHERTYPE_IPX, b"ipx");
    let frame = encap(&eth, &to_ds(None, None)).unwrap();
    assert_eq!(frame[0], 0x08);
    assert_eq!(&frame[24..30], &BRIDGE_TUNNEL);
    let h = DataHeader::parse(&frame).unwrap();
    assert_eq!((h.len, h.qos), (24, None));
    let mut out = Vec::new();
    // A station receives From-DS frames; build one the AP's way.
    let from = encap(
        &ethernet(&STA, &GW, ETHERTYPE_IPX, b"ipx"),
        &Encap {
            direction: Direction::FromDs,
            ..to_ds(None, None)
        },
    )
    .unwrap();
    let h = DataHeader::parse(&from).unwrap();
    decap(&h, &from[h.len..], &mut out).unwrap();
    assert_eq!(out, vec![ethernet(&STA, &GW, ETHERTYPE_IPX, b"ipx")]);
}

#[test]
fn round_trip_with_security_header() {
    let eth = ethernet(&STA, &GW, 0x86DD, &[0x6F, 0xE0, 0, 0, 9, 9]);
    let ccmp = ccmp::header(0x1_0000_0001, 1);
    let how = Encap {
        direction: Direction::FromDs,
        ..to_ds(Some(user_priority(&eth)), Some(ccmp))
    };
    assert_eq!(how.tid, Some(7), "IPv6 traffic class 0xFE is precedence 7");
    let frame = encap(&eth, &how).unwrap();
    let h = DataHeader::parse(&frame).unwrap();
    assert!(h.protected() && h.from_ds());
    let (pn, key) = ccmp::parse_header(&frame[h.len..]).unwrap();
    assert_eq!((pn, key), (0x1_0000_0001, 1));
    let mut out = Vec::new();
    decap(&h, &frame[h.len + ccmp::HEADER_LEN..], &mut out).unwrap();
    assert_eq!(out, vec![eth]);
}

#[test]
fn encap_refusals() {
    assert_eq!(encap(&[0; 13], &to_ds(None, None)), Err(Error::Short));
    let length_field = ethernet(&GW, &STA, 46, &[0; 46]);
    assert_eq!(
        encap(&length_field, &to_ds(None, None)),
        Err(Error::BadEthernet)
    );
    let huge = ethernet(&GW, &STA, 0x0800, &[0; MAX_MSDU]);
    assert_eq!(encap(&huge, &to_ds(None, None)), Err(Error::BadEthernet));
}

#[test]
fn priorities() {
    assert_eq!(user_priority(&ethernet(&GW, &STA, ETHERTYPE_EAPOL, &[])), 7);
    assert_eq!(user_priority(&ethernet(&GW, &STA, 0x0800, &[0x45, 0x20])), 1);
    assert_eq!(user_priority(&ethernet(&GW, &STA, 0x8100, &[0xA0, 0])), 5);
    assert_eq!(user_priority(&ethernet(&GW, &STA, 0x0806, &[0; 28])), 0);
    assert_eq!(user_priority(&ethernet(&GW, &STA, 0x0800, &[])), 0);
}

fn from_ds_header(qos: u16) -> Vec<u8> {
    let mut f = vec![0x88, fc::FROM_DS, 0, 0];
    f.extend_from_slice(&STA);
    f.extend_from_slice(&AP);
    f.extend_from_slice(&GW);
    f.extend_from_slice(&[0x10, 0]);
    f.extend_from_slice(&qos.to_le_bytes());
    f
}

#[test]
fn amsdu_decap_and_injection() {
    let a = ethernet(&STA, &GW, 0x0800, b"first");
    let b = ethernet(&[0xFF; 6], &GW, 0x0806, b"second!");
    let mut frame = from_ds_header(QOS_AMSDU_PRESENT);
    frame.extend_from_slice(&amsdu_body(&[&a, &b]).unwrap());
    let h = DataHeader::parse(&frame).unwrap();
    assert!(h.amsdu());
    let mut out = Vec::new();
    decap(&h, &frame[h.len..], &mut out).unwrap();
    assert_eq!(out, vec![a.clone(), b]);

    // A plain frame with the A-MSDU bit flipped: its LLC header is read as
    // the first subframe's DA, which is refused.
    let plain = encap(
        &a,
        &Encap {
            direction: Direction::FromDs,
            ..to_ds(Some(0), None)
        },
    )
    .unwrap();
    let mut flipped = plain.clone();
    flipped[24] |= QOS_AMSDU_PRESENT as u8;
    let h = DataHeader::parse(&flipped).unwrap();
    assert_eq!(
        decap(&h, &flipped[h.len..], &mut out),
        Err(Error::BadAmsdu)
    );

    // Truncated and over-long subframes drop the whole A-MSDU.
    let mut cut = from_ds_header(QOS_AMSDU_PRESENT);
    cut.extend_from_slice(&amsdu_body(&[&a, &a]).unwrap()[..30]);
    let h = DataHeader::parse(&cut).unwrap();
    let before = out.len();
    assert_eq!(decap(&h, &cut[h.len..], &mut out), Err(Error::BadAmsdu));
    assert_eq!(out.len(), before);
}

#[test]
fn decap_refusals() {
    let mut frame = from_ds_header(0);
    frame.extend_from_slice(&[0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00, 0x80, 0xF3, 1]);
    let h = DataHeader::parse(&frame).unwrap();
    let mut out = Vec::new();
    assert_eq!(decap(&h, &frame[h.len..], &mut out), Err(Error::BadLlc));
    assert_eq!(decap(&h, &[0x42; 20], &mut out), Err(Error::BadLlc));
    assert_eq!(decap(&h, &[0xAA; 5], &mut out), Err(Error::Short));
    let mut frag = from_ds_header(0);
    frag[22] = 0x11;
    let h = DataHeader::parse(&frag).unwrap();
    assert_eq!(decap(&h, &RFC1042, &mut out), Err(Error::Fragmented));
    let mut null = from_ds_header(0);
    null[0] = 0xC8;
    let h = DataHeader::parse(&null).unwrap();
    decap(&h, &[], &mut out).unwrap();
    assert!(out.is_empty());
}

#[test]
fn header_variants() {
    assert_eq!(DataHeader::parse(&[0x80, 0]), Err(Error::NotData));
    assert_eq!(DataHeader::parse(&[0x09, 0]), Err(Error::BadVersion));
    assert_eq!(DataHeader::parse(&[0x08, 0, 0]), Err(Error::Short));
    let mut wds = vec![0x88, fc::TO_DS | fc::FROM_DS | fc::ORDER, 0, 0];
    wds.extend_from_slice(&[1; 26]);
    wds.extend_from_slice(&[7, 0]);
    wds.extend_from_slice(&[0; 4]);
    let h = DataHeader::parse(&wds).unwrap();
    assert_eq!((h.len, h.bssid(), h.addr4), (36, None, Some([1; 6])));
    wds.truncate(35);
    assert_eq!(DataHeader::parse(&wds), Err(Error::Short));
}

/// IEEE 802.11-2020 Annex J.6.4: the CCMP test MPDU's header, nonce and AAD.
#[test]
fn ccmp_annex_j_framing() {
    let header = [
        0x08, 0x48, 0xC3, 0x2C, 0x0F, 0xD2, 0xE1, 0x28, 0xA5, 0x7C, 0x50, 0x30, 0xF1, 0x84, 0x44,
        0x08, 0xAB, 0xAE, 0xA5, 0xB8, 0xFC, 0xBA, 0x80, 0x33,
    ];
    let pn = 0xB503_9776_E70C;
    assert_eq!(
        ccmp::header(pn, 0),
        [0x0C, 0xE7, 0x00, 0x20, 0x76, 0x97, 0x03, 0xB5]
    );
    let f = ccmp::framing(&header).unwrap();
    assert_eq!(
        f.aad,
        [
            0x08, 0x40, 0x0F, 0xD2, 0xE1, 0x28, 0xA5, 0x7C, 0x50, 0x30, 0xF1, 0x84, 0x44, 0x08,
            0xAB, 0xAE, 0xA5, 0xB8, 0xFC, 0xBA, 0x00, 0x00
        ]
    );
    assert_eq!(
        f.nonce(pn),
        [0x00, 0x50, 0x30, 0xF1, 0x84, 0x44, 0x08, 0xB5, 0x03, 0x97, 0x76, 0xE7, 0x0C]
    );
    assert_eq!(f.header_len, 24);
}

#[test]
fn ccmp_qos_and_management_framing() {
    let mut frame = from_ds_header(0x00A5);
    // QoS Data + CF-Ack (subtype 9): b4-b6 of the subtype masked, b7 kept.
    frame[0] = 0x98;
    frame[1] |= fc::RETRY | fc::ORDER | fc::MORE_DATA;
    frame.extend_from_slice(&[0; 4]);
    let f = ccmp::framing(&frame).unwrap();
    assert_eq!(f.aad.len(), 24);
    assert_eq!(f.aad[0], 0x88, "subtype bits 4-6 masked");
    assert_eq!(f.aad[1], fc::FROM_DS | fc::PROTECTED);
    assert_eq!(&f.aad[22..], &[5, 0], "only the TID of QoS Control");
    assert_eq!((f.nonce_flags, f.header_len), (5, 30));
    let deauth = crate::build::deauth(&STA, &AP, &AP, 3, 7);
    let f = ccmp::framing(&deauth).unwrap();
    assert_eq!((f.aad[0], f.nonce_flags), (0xC0, 0x10));
    assert_eq!(ccmp::framing(&[0xD4, 0, 0, 0]), Err(Error::NotData));
}

#[test]
fn ccmp_header_refusals() {
    assert_eq!(ccmp::parse_header(&[0; 7]), Err(Error::BadCcmp));
    assert_eq!(ccmp::parse_header(&[1, 0, 0, 0, 0, 0, 0, 0]), Err(Error::BadCcmp));
    assert_eq!(
        ccmp::parse_header(&ccmp::header(ccmp::PN_MAX, 3)),
        Ok((ccmp::PN_MAX, 3))
    );
    assert_eq!(ccmp::rsc_from_bytes(&[1, 2]), Ok(0x0201));
    assert_eq!(ccmp::rsc_from_bytes(&[0; 9]), Err(Error::BadCcmp));
    assert_eq!(ccmp::rsc_from_bytes(&[0xFF; 8]), Ok(ccmp::PN_MAX));
}

#[test]
fn replay_window_per_tid() {
    let mut r = Replay::new(5);
    assert!(!r.accept(Some(0), 5), "the RSC itself was used");
    assert!(r.accept(Some(0), 6));
    assert!(!r.accept(Some(0), 6), "a repeat");
    assert!(r.accept(Some(3), 6), "TIDs count separately");
    assert!(r.accept(None, 100));
    assert!(!r.accept(None, 99), "an older PN");
    assert!(r.accept(Some(0), 1000));
    assert!(!r.fresh(Some(0), 999));
    assert!(!r.fresh(Some(1), ccmp::PN_MAX + 1));
    assert_eq!(r.last(Some(3)), 6);
    assert_eq!(Replay::slot(Some(0x1F)), 15);
}

#[test]
fn classes() {
    let beacon = crate::build::beacon(
        &AP,
        1,
        crate::build::BeaconFields {
            timestamp: 0,
            interval: 100,
            capability: 1,
        },
        &[],
    );
    assert_eq!(
        classify(&beacon),
        FrameClass::Mgmt {
            subtype: 8,
            protected: false
        }
    );
    let data = from_ds_header(0);
    assert_eq!(classify(&data), FrameClass::Data { protected: false });
    let mut null = data.clone();
    null[0] = 0xC8;
    assert_eq!(classify(&null), FrameClass::NullData);
    assert_eq!(classify(&[0xD4, 0, 0, 0]), FrameClass::Control);
    assert_eq!(classify(&[0x08, 0]), FrameClass::Ignore);
    assert_eq!(classify(&[0x0B, 0]), FrameClass::Ignore);
    assert_eq!(classify(&[]), FrameClass::Ignore);
    let mut reserved = data;
    reserved[0] = 0x0C;
    assert_eq!(classify(&reserved), FrameClass::Ignore);
    let eapol = ethernet(&STA, &AP, ETHERTYPE_EAPOL, &[2, 3, 0, 0]);
    assert_eq!(classify_msdu(&eapol), MsduClass::Eapol);
    assert_eq!(classify_msdu(&eapol[..12]), MsduClass::Other);
}
