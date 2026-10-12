//! The data path: Ethernet to 802.11 and back, EAPOL wrapping, CCMP headers
//! and packet numbers, the replay window, the controlled port.

use std::vec::Vec;

use ieee80211::data::{fc, DataHeader, ETHERTYPE_EAPOL, RFC1042};

use super::*;
use crate::{DataDrop, Event, FrameKind};

const IP: u16 = 0x0800;

fn eapol_from_sta() -> Vec<u8> {
    ethernet(&AP, &STA, ETHERTYPE_EAPOL, &[2, 3, 0, 95, 2, 1, 2])
}

fn last_sent(m: &SoftMac<FakeRadio>) -> (Vec<u8>, TxOptions) {
    m.radio().sent.last().cloned().unwrap()
}

#[test]
fn open_network_data_both_ways() {
    let mut m = open_authorized();
    let eth = ethernet(&GW, &STA, IP, &[0x45, 0xA0, 1, 2]);
    m.send_ethernet(&eth).unwrap();
    let (frame, options) = last_sent(&m);
    let h = DataHeader::parse(&frame).unwrap();
    assert!(h.to_ds() && !h.protected());
    assert_eq!((h.addr1, h.addr2, h.addr3, h.tid()), (AP, STA, GW, Some(5)));
    assert_eq!(&frame[h.len..h.len + 6], &RFC1042);
    assert!(options.data && !options.protected && !options.basic_rate);
    assert_eq!(options.priority, 5);

    let down = ethernet(&STA, &GW, IP, b"reply");
    receive(&mut m, from_ap(&down, Some(0), None, 0), 100);
    assert_eq!(m.next_ethernet(), Some(down));
    assert_eq!(m.next_ethernet(), None);
    assert_eq!((m.stats().tx_data, m.stats().rx_data), (1, 1));
}

#[test]
fn a_bss_without_qos_gets_plain_data() {
    let mut m = joined(false);
    m.set_state(OWNER, 2, 1).unwrap();
    m.set_state(OWNER, 3, 1).unwrap();
    m.send_ethernet(&ethernet(&GW, &STA, IP, &[0x45, 0]))
        .unwrap();
    let (frame, _) = last_sent(&m);
    assert_eq!(frame[0], 0x08);
    // A beacon that shows WMM switches it to QoS data.
    let mut ies = beacon(&AP, b"LazyNet", 6, false);
    ies.extend_from_slice(&[221, 7, 0x00, 0x50, 0xF2, 0x02, 0x00, 0x01, 0x00]);
    hear(&mut m, ies, 6);
    m.poll(50);
    m.send_ethernet(&ethernet(&GW, &STA, IP, &[0x45, 0]))
        .unwrap();
    assert_eq!(last_sent(&m).0[0], 0x88);
}

#[test]
fn the_port_is_closed_until_authorized() {
    let mut m = joined(true);
    m.set_state(OWNER, 2, 1).unwrap();
    let eth = ethernet(&GW, &STA, IP, b"x");
    assert_eq!(m.send_ethernet(&eth), Err(DataDrop::PortClosed));
    let down = ethernet(&STA, &GW, IP, b"early");
    receive(&mut m, from_ap(&down, Some(0), None, 0), 30);
    assert_eq!(m.next_ethernet(), None);
    // EAPOL from the AP reaches the owner at any time, never the stack.
    let eapol = ethernet(&STA, &AP, ETHERTYPE_EAPOL, &[2, 3, 0, 95]);
    receive(&mut m, from_ap(&eapol, Some(7), None, 0), 40);
    assert_eq!(
        m.next_event(),
        Some(Event::RxMgmt {
            kind: FrameKind::Eapol,
            rssi_dbm: -50,
            frame: eapol.clone()
        })
    );
    m.set_state(OWNER, 3, 1).unwrap();
    receive(&mut m, from_ap(&eapol, Some(7), None, 0), 50);
    assert_eq!(m.next_ethernet(), None);
    assert!(matches!(m.next_event(), Some(Event::RxMgmt { .. })));
    // The stack may not send EAPOL, nor another source address.
    assert_eq!(m.send_ethernet(&eapol_from_sta()), Err(DataDrop::Refused));
    assert_eq!(
        m.send_ethernet(&ethernet(&GW, &AP2, IP, b"x")),
        Err(DataDrop::Refused)
    );
    assert_eq!(m.send_ethernet(&[0; 13]), Err(DataDrop::Runt));
    assert_eq!(
        m.send_ethernet(&ethernet(&GW, &STA, IP, &[0; 1443])),
        Err(DataDrop::TooLong)
    );
    m.send_ethernet(&ethernet(&GW, &STA, IP, &[0; 1442]))
        .unwrap();
    m.radio_mut().queue_full = true;
    assert_eq!(m.send_ethernet(&eth), Err(DataDrop::Full));
}

#[test]
fn eapol_goes_in_the_clear_before_the_pairwise_key_and_protected_after() {
    let mut m = joined(true);
    m.set_state(OWNER, 2, 1).unwrap();
    m.tx_mgmt(OWNER, FrameKind::Eapol as u32, &eapol_from_sta())
        .unwrap();
    let (frame, options) = last_sent(&m);
    let h = DataHeader::parse(&frame).unwrap();
    assert!(!h.protected() && h.to_ds());
    assert_eq!(h.tid(), Some(7));
    assert_eq!(&frame[h.len + 6..h.len + 8], &[0x88, 0x8E]);
    assert!(options.basic_rate && options.data && !options.protected);

    m.set_key(OWNER, &key(KeyKind::Pairwise, 0, &[])).unwrap();
    for expected_pn in 1..=2u64 {
        m.tx_mgmt(OWNER, 1, &eapol_from_sta()).unwrap();
        let (frame, options) = last_sent(&m);
        let h = DataHeader::parse(&frame).unwrap();
        assert!(h.protected() && options.protected);
        let (pn, key_id) = ccmp::parse_header(&frame[h.len..]).unwrap();
        assert_eq!((pn, key_id), (expected_pn, 0));
        assert_eq!(&frame[h.len + 8 + 6..h.len + 8 + 8], &[0x88, 0x8E]);
    }
    // Refused: not EAPOL, too short, another source.
    let not_eapol = ethernet(&AP, &STA, IP, &[0; 8]);
    let short = ethernet(&AP, &STA, ETHERTYPE_EAPOL, &[2, 3]);
    let spoofed = ethernet(&AP, &AP2, ETHERTYPE_EAPOL, &[2, 3, 0, 0]);
    for bad in [not_eapol, short, spoofed] {
        assert_eq!(m.tx_mgmt(OWNER, 1, &bad), Err(crate::Error::Invalid));
    }
}

#[test]
fn protected_data_uses_one_packet_number_sequence_per_key() {
    let mut m = wpa2_authorized();
    let mut pns = Vec::new();
    for tos in [0x00u8, 0xE0, 0x20] {
        m.send_ethernet(&ethernet(&GW, &STA, IP, &[0x45, tos]))
            .unwrap();
        let (frame, options) = last_sent(&m);
        assert!(options.protected);
        let h = DataHeader::parse(&frame).unwrap();
        pns.push(ccmp::parse_header(&frame[h.len..]).unwrap().0);
    }
    assert_eq!(pns, [1, 2, 3]);
}

fn last_pn(m: &SoftMac<FakeRadio>) -> u64 {
    let (frame, _) = last_sent(m);
    let h = DataHeader::parse(&frame).unwrap();
    ccmp::parse_header(&frame[h.len..]).unwrap().0
}

#[test]
fn a_rekey_never_starts_the_packet_numbers_over() {
    let mut m = wpa2_authorized();
    let send = |m: &mut SoftMac<FakeRadio>| {
        m.send_ethernet(&ethernet(&GW, &STA, IP, &[0x45, 0]))
            .unwrap()
    };
    send(&mut m);
    send(&mut m);
    // A PTK rekey (or the same key installed again) carries the PN on: a
    // temporal key installed twice never sees a PN twice.
    m.set_key(OWNER, &key(KeyKind::Pairwise, 0, &[])).unwrap();
    send(&mut m);
    assert_eq!(last_pn(&m), 3);
    // So does a key deleted and installed again.
    m.del_key(OWNER, KeyKind::Pairwise as u32, 0).unwrap();
    m.set_key(OWNER, &key(KeyKind::Pairwise, 0, &[])).unwrap();
    send(&mut m);
    assert_eq!(last_pn(&m), 4);
    // EAPOL under the key shares the sequence.
    let eapol = ethernet(&AP, &STA, ETHERTYPE_EAPOL, &[2, 3, 0, 95, 2, 1, 2]);
    m.tx_mgmt(OWNER, FrameKind::Eapol as u32, &eapol).unwrap();
    assert_eq!(last_pn(&m), 5);
    // An EAPOL frame too long for the radio is refused before it takes one.
    let mut long = ethernet(&AP, &STA, ETHERTYPE_EAPOL, &[2, 3, 0, 95]);
    long.resize(1484 - 10, 0);
    assert_eq!(
        m.tx_mgmt(OWNER, FrameKind::Eapol as u32, &long),
        Err(crate::Error::Invalid)
    );
    send(&mut m);
    assert_eq!(last_pn(&m), 6);
    // Only the end of the association starts them over.
    m.leave(OWNER, 3).unwrap();
    let mut m = wpa2_authorized();
    send(&mut m);
    assert_eq!(last_pn(&m), 1);
}

#[test]
fn received_protected_data_is_checked_for_replays() {
    let mut m = wpa2_authorized();
    let eth = ethernet(&STA, &GW, IP, b"payload");
    receive(&mut m, from_ap(&eth, Some(0), Some(10), 0), 100);
    assert_eq!(
        m.next_ethernet(),
        Some(eth.clone()),
        "the kept MIC is stripped"
    );
    for (tid, pn) in [(Some(0), 10), (Some(0), 9)] {
        receive(&mut m, from_ap(&eth, tid, Some(pn), 0), 110);
        assert_eq!(m.next_ethernet(), None);
    }
    assert_eq!(m.stats().replays, 2);
    // Another TID has its own counter.
    receive(&mut m, from_ap(&eth, Some(5), Some(3), 0), 120);
    assert_eq!(m.next_ethernet(), Some(eth.clone()));
    // A frame the radio did not decrypt, or under an unknown key id.
    let mut undecrypted = from_ap(&eth, Some(0), Some(50), 0);
    undecrypted.decrypted = false;
    receive(&mut m, undecrypted, 130);
    receive(&mut m, from_ap(&eth, Some(0), Some(51), 1), 140);
    assert_eq!(m.next_ethernet(), None);
    // Plaintext in a protected BSS.
    receive(&mut m, from_ap(&eth, Some(0), None, 0), 150);
    assert_eq!(m.next_ethernet(), None);
}

#[test]
fn group_frames_use_the_group_key_window() {
    let mut m = wpa2_authorized();
    let bcast = ethernet(&[0xFF; 6], &GW, 0x0806, &[0; 28]);
    // Group key 1 was installed with RSC 5: PN 5 is old, 6 is new.
    receive(&mut m, from_ap(&bcast, None, Some(5), 1), 100);
    assert_eq!(m.next_ethernet(), None);
    receive(&mut m, from_ap(&bcast, None, Some(6), 1), 110);
    assert_eq!(m.next_ethernet(), Some(bcast.clone()));
    receive(&mut m, from_ap(&bcast, None, Some(7), 2), 120);
    assert_eq!(m.next_ethernet(), None, "no group key 2");
    // Our own broadcast reflected by the AP is not passed up.
    let mine = ethernet(&[0xFF; 6], &STA, 0x0806, &[0; 28]);
    receive(&mut m, from_ap(&mine, None, Some(8), 1), 130);
    assert_eq!(m.next_ethernet(), None);
}

#[test]
fn data_not_from_our_bss_or_not_for_us_is_dropped() {
    let mut m = open_authorized();
    let eth = ethernet(&STA, &GW, IP, b"x");
    let mut other_bss = from_ap(&eth, Some(0), None, 0);
    other_bss.frame[10..16].copy_from_slice(&AP2);
    let mut to_other = from_ap(&ethernet(&GW, &AP2, IP, b"x"), Some(0), None, 0);
    to_other.frame[4..10].copy_from_slice(&AP2);
    let mut to_ds = from_ap(&eth, Some(0), None, 0);
    to_ds.frame[1] = fc::TO_DS;
    let mut garbled = from_ap(&eth, Some(0), None, 0);
    garbled.frame.truncate(30);
    let mut fake_trailer = from_ap(&eth, Some(0), None, 0);
    fake_trailer.trailer = 4;
    for rx in [other_bss, to_other, to_ds, garbled, fake_trailer] {
        receive(&mut m, rx, 100);
    }
    assert_eq!(m.next_ethernet(), None);
    assert_eq!(m.stats().rx_data_dropped, 5);
}

#[test]
fn a_lower_state_or_leave_empties_the_receive_queue() {
    let mut m = open_authorized();
    let eth = ethernet(&STA, &GW, IP, b"queued");
    receive(&mut m, from_ap(&eth, Some(0), None, 0), 100);
    m.set_state(OWNER, 2, 1).unwrap();
    assert_eq!(m.next_ethernet(), None);
    let mut m = open_authorized();
    for _ in 0..300 {
        m.radio_mut()
            .inbound
            .push_back(from_ap(&eth, Some(0), None, 0));
    }
    for t in 0..10 {
        m.poll(100 + t);
    }
    let mut queued = 0;
    while m.next_ethernet().is_some() {
        queued += 1;
    }
    assert_eq!(queued, crate::mac::RX_QUEUE, "the queue is bounded");
    assert_eq!(m.stats().rx_data_dropped, 300 - queued as u64);
}
