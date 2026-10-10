use std::vec::Vec;

use super::*;
use crate::key::{info, KeyFrame, FIXED_LEN};
use crate::keydata::{self, Gtk};
use crate::Error;

fn sample() -> Vec<u8> {
    raw_frame(
        2,
        0x0088,
        16,
        0x0102_0304_0506_0708,
        &[9; 32],
        [1; 8],
        &[0xAB; 22],
        [3; 16],
    )
}

#[test]
fn parse_reads_every_field_at_the_standard_offsets() {
    let frame = KeyFrame::parse(&sample()).unwrap();
    assert_eq!((frame.eapol_version, frame.descriptor), (2, 2));
    assert_eq!(
        (frame.info, frame.version(), frame.flags()),
        (0x0088 | 2, 2, 0x0088)
    );
    assert_eq!(frame.key_length, 16);
    assert_eq!(frame.replay_counter(), 0x0102_0304_0506_0708);
    assert_eq!(
        (frame.nonce, frame.rsc, frame.mic),
        ([9; 32], [1; 8], [3; 16])
    );
    assert_eq!(frame.key_data, [0xAB; 22]);
    assert_eq!(frame.encode().unwrap(), sample());
    assert!(frame.info & info::ACK != 0 && frame.info & info::MIC == 0);
}

#[test]
fn mic_input_is_the_pdu_with_the_mic_zeroed() {
    let pdu = sample();
    let input = KeyFrame::parse(&pdu).unwrap().mic_input().unwrap();
    let mut expect = pdu;
    expect[81..97].fill(0);
    assert_eq!(input, expect);
}

#[test]
fn trailing_ethernet_padding_is_ignored() {
    let mut pdu = sample();
    pdu.extend_from_slice(&[0; 20]);
    assert_eq!(KeyFrame::parse(&pdu).unwrap().key_data.len(), 22);
}

#[test]
fn every_truncation_is_short() {
    let pdu = sample();
    for end in 0..pdu.len() {
        assert_eq!(
            KeyFrame::parse(&pdu[..end]).unwrap_err(),
            Error::Short,
            "end {end}"
        );
    }
}

#[test]
fn malformed_headers_are_refused() {
    let mut not_key = sample();
    not_key[1] = 0; // EAP packet
    assert_eq!(KeyFrame::parse(&not_key).unwrap_err(), Error::NotKey);
    // Body length and key data length must agree, both ways.
    let mut longer = sample();
    longer[97..99].copy_from_slice(&21u16.to_be_bytes());
    assert_eq!(KeyFrame::parse(&longer).unwrap_err(), Error::BadLength);
    let mut shorter = sample();
    shorter[97..99].copy_from_slice(&23u16.to_be_bytes());
    assert_eq!(KeyFrame::parse(&shorter).unwrap_err(), Error::BadLength);
    // A body length smaller than the fixed part.
    let mut tiny = sample();
    tiny[2..4].copy_from_slice(&10u16.to_be_bytes());
    assert_eq!(KeyFrame::parse(&tiny).unwrap_err(), Error::Short);
    // A key data length past the buffer.
    let mut huge = sample();
    huge[2..4].copy_from_slice(&0xFFF0u16.to_be_bytes());
    assert_eq!(KeyFrame::parse(&huge).unwrap_err(), Error::Short);
    assert_eq!(FIXED_LEN, 99);
}

#[test]
fn key_data_over_64k_cannot_be_framed() {
    let mut frame = KeyFrame::zeroed(2);
    frame.key_data = std::vec![0; 70_000];
    assert_eq!(frame.encode().unwrap_err(), Error::BadLength);
}

#[test]
fn key_data_walks_rsn_and_gtk_with_padding() {
    let ie = rsn_ie(ieee80211::Akm::Psk);
    let plain = msg3_plain(&ie, 3, &[0x55; 16]);
    assert_eq!(plain.len() % 8, 0);
    let data = keydata::parse(&plain).unwrap();
    assert_eq!(data.rsn_ie, Some(&ie[..]));
    assert_eq!(
        data.gtk,
        Some(Gtk {
            index: 3,
            tx: false,
            key: &[0x55; 16]
        })
    );
    // The Tx bit.
    let mut tx = plain.clone();
    tx[ie.len() + 6] |= 0x04;
    assert!(keydata::parse(&tx).unwrap().gtk.unwrap().tx);
}

#[test]
fn padding_forms() {
    // A lone trailing dd, dd 00 and zeros, and no padding at all.
    let kde = [
        0xDD, 22, 0, 0x0F, 0xAC, 1, 1, 0, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
    ];
    assert!(keydata::parse(&kde).unwrap().gtk.is_some());
    for tail in [&[0xDD][..], &[0xDD, 0][..], &[0xDD, 0, 0, 0, 0, 0, 0][..]] {
        let data = [&kde[..], tail].concat();
        assert!(keydata::parse(&data).unwrap().gtk.is_some());
    }
    // Garbage after the padding marker is refused.
    assert_eq!(
        keydata::parse(&[0xDD, 0, 0, 1]).unwrap_err(),
        Error::BadKeyData
    );
    assert_eq!(keydata::parse(&[]).unwrap(), Default::default());
}

#[test]
fn truncated_and_malformed_kdes_are_refused() {
    let ie = rsn_ie(ieee80211::Akm::Psk);
    let plain = msg3_plain(&ie, 1, &[1; 16]);
    // Cutting inside an element is BadKeyData; cutting at an element boundary
    // is a shorter, valid list.
    for end in 1..ie.len() + 24 {
        // A boundary is a valid shorter list; so is a lone trailing dd (padding).
        if end == ie.len() || end == ie.len() + 1 {
            continue;
        }
        assert_eq!(
            keydata::parse(&plain[..end]).unwrap_err(),
            Error::BadKeyData,
            "end {end}"
        );
    }
    // A lone ID octet that is not the padding marker.
    assert_eq!(keydata::parse(&[48]).unwrap_err(), Error::BadKeyData);
    // A vendor KDE too short for OUI and type.
    assert_eq!(
        keydata::parse(&[0xDD, 3, 0, 0x0F, 0xAC]).unwrap_err(),
        Error::BadKeyData
    );
    // A second GTK KDE.
    let gtk = [
        0xDD, 22, 0, 0x0F, 0xAC, 1, 1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
    ];
    assert_eq!(
        keydata::parse(&[&gtk[..], &gtk].concat()).unwrap_err(),
        Error::BadKeyData
    );
    // Other OUIs and unknown KDE types are skipped.
    let other = [
        0xDD, 6, 0x00, 0x50, 0xF2, 1, 0, 0, 0xDD, 6, 0x00, 0x0F, 0xAC, 4, 0, 0,
    ];
    assert_eq!(keydata::parse(&other).unwrap(), Default::default());
    // Only the first RSN element counts.
    let two = [&ie[..], &ie[..]].concat();
    assert_eq!(keydata::parse(&two).unwrap().rsn_ie, Some(&ie[..]));
}
