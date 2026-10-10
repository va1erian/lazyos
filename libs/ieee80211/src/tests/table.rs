use std::vec::Vec;

use super::{beacon_ies, AP};
use crate::bss::{Bss, Security};
use crate::build::{self, BeaconFields};
use crate::frame::Mgmt;
use crate::ie::IeBuilder;
use crate::table::{ScanTable, Update};
use crate::{cap, Rsn};

fn fields(capability: u16) -> BeaconFields {
    BeaconFields {
        timestamp: 1,
        interval: 100,
        capability,
    }
}

fn bss_from(frame: &[u8], channel: u8, rssi: i8, now: u64) -> Option<Bss> {
    Bss::from_frame(&Mgmt::parse(frame).unwrap(), channel, rssi, now)
}

#[test]
fn bss_from_a_wpa2_beacon() {
    let ies = beacon_ies(b"LazyNet", 6);
    let frame = build::beacon(&AP, 1, fields(cap::ESS | cap::PRIVACY), &ies);
    let bss = bss_from(&frame, 6, -52, 1000).unwrap();
    assert_eq!((bss.bssid, bss.channel, bss.rssi), (AP, 6, -52));
    assert_eq!(bss.ssid_lossy(), "LazyNet");
    assert_eq!(bss.security, Security::Rsn(Rsn::wpa2_psk()));
    assert_eq!(
        bss.rsn_ie.as_deref(),
        Some(&Rsn::wpa2_psk().to_ie().unwrap()[..])
    );
    assert!(bss.ht && !bss.vht && !bss.he && bss.is_rsn());
    assert_eq!((bss.country, bss.rates.len()), (Some(*b"US"), 10));
    assert!(!bss.from_probe_response);
}

#[test]
fn channel_comes_from_the_chip_then_the_ds_element() {
    let ies = beacon_ies(b"x", 11);
    let frame = build::beacon(&AP, 1, fields(cap::ESS), &ies);
    assert_eq!(bss_from(&frame, 1, -40, 0).unwrap().channel, 1);
    let bss = bss_from(&frame, 0, -40, 0).unwrap();
    assert_eq!((bss.channel, bss.ds_channel), (11, Some(11)));
}

#[test]
fn security_summaries() {
    let open = IeBuilder::new().ssid(b"o").unwrap().finish();
    let f = build::beacon(&AP, 1, fields(cap::ESS), &open);
    assert_eq!(bss_from(&f, 1, 0, 0).unwrap().security, Security::Open);
    let f = build::beacon(&AP, 1, fields(cap::ESS | cap::PRIVACY), &open);
    assert_eq!(bss_from(&f, 1, 0, 0).unwrap().security, Security::Wep);
    let wpa = IeBuilder::new()
        .raw(221, &[0x00, 0x50, 0xF2, 1, 1, 0])
        .unwrap()
        .finish();
    let f = build::beacon(&AP, 1, fields(cap::ESS | cap::PRIVACY), &wpa);
    assert_eq!(bss_from(&f, 1, 0, 0).unwrap().security, Security::Wpa1);
    let bad = IeBuilder::new().raw(48, &[9, 9]).unwrap().finish();
    let f = build::beacon(&AP, 1, fields(cap::ESS | cap::PRIVACY), &bad);
    let bss = bss_from(&f, 1, 0, 0).unwrap();
    assert_eq!(bss.security, Security::InvalidRsn);
    assert!(!bss.is_rsn());
}

#[test]
fn frames_that_are_not_a_usable_bss() {
    let ies = beacon_ies(b"x", 1);
    // IBSS, no ESS, spoofed transmitter, group BSSID, zero BSSID.
    let f = build::beacon(&AP, 1, fields(cap::IBSS), &ies);
    assert!(bss_from(&f, 1, 0, 0).is_none());
    let f = build::beacon(&AP, 1, fields(0), &ies);
    assert!(bss_from(&f, 1, 0, 0).is_none());
    let mut f = build::beacon(&AP, 1, fields(cap::ESS), &ies);
    f[10] ^= 1; // addr2 differs from the BSSID
    assert!(bss_from(&f, 1, 0, 0).is_none());
    for bad in [[0x03, 0, 0, 0, 0, 1], [0; 6]] {
        let f = build::beacon(&bad, 1, fields(cap::ESS), &ies);
        assert!(bss_from(&f, 1, 0, 0).is_none());
    }
    // An over-long SSID refuses the frame; a deauth is not a BSS.
    let mut long = Vec::new();
    crate::ie::push_ie(&mut long, 0, &[b'a'; 40]).unwrap();
    let f = build::beacon(&AP, 1, fields(cap::ESS), &long);
    assert!(bss_from(&f, 1, 0, 0).is_none());
    assert!(bss_from(&build::deauth(&AP, &AP, &AP, 1, 1), 1, 0, 0).is_none());
}

#[test]
fn hidden_beacon_does_not_blank_a_name_learned_from_a_probe_response() {
    let named = build::probe_response(&AP, &AP, 1, fields(cap::ESS), &beacon_ies(b"secret", 1));
    let hidden = build::beacon(&AP, 2, fields(cap::ESS), &beacon_ies(&[0; 6], 1));
    let mut table = ScanTable::new(4);
    table.update(bss_from(&named, 1, -60, 10).unwrap());
    assert!(table.get(&AP).unwrap().from_probe_response);
    assert_eq!(
        table.update(bss_from(&hidden, 1, -55, 20).unwrap()),
        Update::Updated
    );
    let bss = table.get(&AP).unwrap();
    assert_eq!(
        (bss.ssid_lossy().as_str(), bss.rssi, bss.last_seen),
        ("secret", -55, 20)
    );
    let only_hidden = bss_from(&hidden, 1, -55, 20).unwrap();
    assert!(only_hidden.hidden && only_hidden.ssid_lossy().is_empty());
}

fn synthetic(id: u8, rssi: i8, at: u64) -> Bss {
    let ies = beacon_ies(&[b'a', id], 1);
    let frame = build::beacon(&[2, 0, 0, 0, 0, id], 1, fields(cap::ESS), &ies);
    bss_from(&frame, 1, rssi, at).unwrap()
}

#[test]
fn capacity_is_a_hard_bound_and_the_strongest_survive() {
    let mut table = ScanTable::new(3);
    for (id, rssi) in [(1, -70), (2, -60), (3, -50)] {
        assert_eq!(table.update(synthetic(id, rssi, 0)), Update::Inserted);
    }
    // Weaker than everything: dropped. Equal to the weakest: dropped.
    assert_eq!(table.update(synthetic(4, -80, 1)), Update::Dropped);
    assert_eq!(table.update(synthetic(5, -70, 1)), Update::Dropped);
    // Stronger than the weakest: replaces it.
    assert_eq!(table.update(synthetic(6, -40, 1)), Update::Inserted);
    assert_eq!(table.len(), 3);
    assert!(table.get(&[2, 0, 0, 0, 0, 1]).is_none());
    // A BSSID already present always updates, even full.
    assert_eq!(table.update(synthetic(2, -90, 2)), Update::Updated);
    let order: Vec<i8> = table.by_signal().iter().map(|b| b.rssi).collect();
    assert_eq!(order, [-40, -50, -90]);
    assert_eq!(
        ScanTable::new(0).update(synthetic(1, 0, 0)),
        Update::Dropped
    );
}

#[test]
fn flood_of_random_bssids_cannot_grow_the_table() {
    let mut table = ScanTable::new(8);
    for id in 0..=255u8 {
        table.update(synthetic(id, -90 + (id % 5) as i8, u64::from(id)));
    }
    assert_eq!(table.len(), 8);
}

#[test]
fn ageing_removes_only_the_stale() {
    let mut table = ScanTable::new(8);
    table.update(synthetic(1, -50, 1_000));
    table.update(synthetic(2, -50, 9_000));
    assert_eq!(table.expire(10_000, 5_000), 1);
    assert_eq!(table.len(), 1);
    assert!(table.get(&[2, 0, 0, 0, 0, 2]).is_some());
    // A clock that went backwards ages nothing.
    assert_eq!(table.expire(0, 0), 0);
    assert_eq!(table.expire(100_000, 5_000), 1);
    assert!(table.is_empty());
}
