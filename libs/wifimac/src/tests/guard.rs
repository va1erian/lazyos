//! The guards around the air: the country table and what a new country does
//! to a scan or a join, nothing sent while away on a scan step or before a
//! passive channel was heard, the addresses `TxMgmt` may use, the order of
//! association states, key installation failures, and the scan table under a
//! flood.

use std::vec;
use std::vec::Vec;

use ieee80211::build::{self, BeaconFields};
use ieee80211::frame::{SUB_DEAUTH, SUB_PROBE_REQ};
use ieee80211::ie::IeBuilder;

use super::*;
use crate::{DataDrop, Error, Event, FrameKind, ScanRequest};

const IP: u16 = 0x0800;

fn auth_from_sta() -> Vec<u8> {
    build::auth(&AP, &STA, &AP, 0, (0, 1, 0), &[])
}

fn join_on(m: &mut SoftMac<FakeRadio>, band: u32, channel: u32, now: u64) -> Result<(), Error> {
    let request = JoinRequest {
        bssid: AP.to_vec(),
        channel,
        band,
        width: 0,
    };
    m.join(OWNER, &request, now)
}

fn heard_on(band: Band, channel: u8, frame: Vec<u8>, rssi_dbm: i8) -> Received {
    Received {
        frame,
        rssi_dbm,
        band,
        channel,
        decrypted: false,
        trailer: 0,
    }
}

#[test]
fn only_real_countries_are_accepted() {
    let mut m = attached();
    for bad in ["ZZ", "UK", "EU", "QQ", "fr", "F", "FRA", "0", "000", ""] {
        assert_eq!(m.set_country(OWNER, bad), Err(Error::Invalid), "{bad}");
    }
    for good in ["00", "US", "JP", "DE", "GB"] {
        m.set_country(OWNER, good).unwrap();
    }
}

#[test]
fn a_new_country_leaves_a_forbidden_channel_silently() {
    let mut m = attached();
    join_on(&mut m, 0, 13, 0).unwrap();
    m.tx_mgmt(OWNER, 0, &auth_from_sta()).unwrap();
    m.set_state(OWNER, 1, 0).unwrap();
    m.radio_mut().sent.clear();
    // Channel 13 is not a US channel: no deauthentication on it.
    m.set_country(OWNER, "US").unwrap();
    assert!(m.radio().sent.is_empty());
    assert_eq!(m.radio().bssid, None);
    assert_eq!(m.next_event(), Some(Event::Deauthenticated { reason: 3 }));
    assert_eq!(
        m.tx_mgmt(OWNER, 0, &auth_from_sta()),
        Err(Error::NotConnected)
    );
}

#[test]
fn a_new_country_replans_a_running_scan() {
    let mut m = attached();
    let request = ScanRequest {
        channels: vec![1, 12, 13, 36],
        band: u32::MAX,
        active: true,
        ..Default::default()
    };
    m.scan(OWNER, &request, 0).unwrap();
    m.set_country(OWNER, "00").unwrap();
    for t in 1..10 {
        m.poll(t * 200);
    }
    let tuned: Vec<u8> = m.radio().tuned.iter().map(|c| c.number).collect();
    assert_eq!(tuned, [1, 12, 13, 36]);
    let probes = m
        .radio()
        .sent
        .iter()
        .filter(|(f, _)| f[0] >> 4 == SUB_PROBE_REQ)
        .count();
    assert_eq!(probes, 1, "the world domain probes on 1 only");

    let mut m = attached();
    m.scan(OWNER, &request, 0).unwrap();
    m.set_country(OWNER, "US").unwrap();
    for t in 1..10 {
        m.poll(t * 200);
    }
    let tuned: Vec<u8> = m.radio().tuned.iter().map(|c| c.number).collect();
    assert_eq!(tuned, [1, 36], "12 and 13 dropped");
}

#[test]
fn nothing_goes_to_the_bss_while_away_on_a_scan_step() {
    let mut m = open_authorized();
    let request = ScanRequest {
        channels: vec![1, 11],
        band: 0,
        dwell_ms: 500,
        ..Default::default()
    };
    m.scan(OWNER, &request, 100).unwrap();
    m.radio_mut().sent.clear();
    assert!(!m.tx_ready(), "the stack's frames wait in its ring");
    let eth = ethernet(&GW, &STA, IP, &[0x45, 0]);
    assert_eq!(m.send_ethernet(&eth), Err(DataDrop::Full));
    m.tx_mgmt(OWNER, 0, &auth_from_sta()).unwrap();
    m.poll(600);
    assert!(m.radio().sent.is_empty(), "held while on 1 and 11");
    m.poll(1100);
    assert_eq!(m.radio().tuned.last().map(|c| c.number), Some(6));
    assert_eq!(m.radio().sent.len(), 1, "sent once back on 6");
    assert!(m.tx_ready());
    m.send_ethernet(&eth).unwrap();
}

#[test]
fn a_passive_channel_is_silent_until_the_bss_is_heard() {
    let mut m = attached();
    // Channel 52 is a DFS channel: passive everywhere.
    join_on(&mut m, 1, 52, 0).unwrap();
    m.tx_mgmt(OWNER, 0, &auth_from_sta()).unwrap();
    m.poll(10);
    assert!(m.radio().sent.is_empty());
    let ies = IeBuilder::new().ssid(b"x").unwrap().finish();
    let fields = BeaconFields {
        timestamp: 1,
        interval: 100,
        capability: 1,
    };
    let beacon = build::beacon(&AP, 1, fields, &ies);
    m.radio_mut()
        .inbound
        .push_back(heard_on(Band::Ghz5, 52, beacon, -60));
    m.poll(20);
    assert_eq!(m.radio().sent.len(), 1, "the held authentication");
}

#[test]
fn txmgmt_frames_go_to_the_bss_only() {
    let mut m = joined(false);
    let to_other = build::auth(&AP2, &STA, &AP, 0, (0, 1, 0), &[]);
    let other_bss = build::auth(&AP, &STA, &AP2, 0, (0, 1, 0), &[]);
    for bad in [to_other, other_bss] {
        assert_eq!(m.tx_mgmt(OWNER, 0, &bad), Err(Error::Invalid));
    }
    let ies = IeBuilder::new().ssid(b"").unwrap().finish();
    let probe = build::probe_request(&STA, 0, &ies);
    m.tx_mgmt(OWNER, 0, &probe).unwrap();
    let eapol_elsewhere = ethernet(&AP2, &STA, 0x888E, &[2, 3, 0, 95]);
    assert_eq!(
        m.tx_mgmt(OWNER, FrameKind::Eapol as u32, &eapol_elsewhere),
        Err(Error::Invalid)
    );
}

#[test]
fn association_states_come_in_order_and_fail_closed() {
    let mut m = joined(false);
    assert_eq!(
        m.set_state(OWNER, 3, 1),
        Err(Error::Invalid),
        "not associated"
    );
    // A BSS that advertised privacy opens the port only with a pairwise key.
    let mut m = attached();
    let request = ScanRequest {
        channels: vec![6],
        band: 0,
        ..Default::default()
    };
    let id = m.scan(OWNER, &request, 0).unwrap();
    let ies = IeBuilder::new().ssid(b"wpa").unwrap().finish();
    let fields = BeaconFields {
        timestamp: 1,
        interval: 100,
        capability: 0x0011,
    };
    hear(&mut m, build::beacon(&AP, 1, fields, &ies), 6);
    m.poll(10);
    m.abort_scan(OWNER, id, 10).unwrap();
    join_on(&mut m, 0, 6, 20).unwrap();
    m.set_state(OWNER, 2, 1).unwrap();
    assert_eq!(m.set_state(OWNER, 3, 1), Err(Error::Invalid), "no key");
    m.set_key(OWNER, &key(KeyKind::Pairwise, 0, &[])).unwrap();
    m.set_key(OWNER, &key(KeyKind::Group, 1, &[])).unwrap();
    m.set_state(OWNER, 3, 1).unwrap();
    // Below Associated the keys go.
    m.set_state(OWNER, 1, 0).unwrap();
    assert!(m.radio().keys.is_empty());
    assert_eq!(m.set_state(OWNER, 2, 1), Ok(()));
    assert_eq!(m.set_state(OWNER, 3, 1), Err(Error::Invalid));
}

#[test]
fn a_key_the_radio_refused_is_not_believed() {
    let mut m = wpa2_authorized();
    m.radio_mut().slots_full = true;
    assert_eq!(
        m.set_key(OWNER, &key(KeyKind::Pairwise, 0, &[])),
        Err(Error::NoSpace)
    );
    assert!(m
        .radio()
        .keys
        .iter()
        .all(|(k, _, _)| *k != KeyKind::Pairwise));
    let eth = ethernet(&GW, &STA, IP, &[0x45, 0]);
    assert_eq!(m.send_ethernet(&eth), Err(DataDrop::PortClosed));
}

#[test]
fn the_scan_table_keeps_the_strongest_copy_and_the_strongest_bsses() {
    let mut m = attached();
    let request = ScanRequest {
        channels: vec![5, 6, 7],
        band: 0,
        ..Default::default()
    };
    let id = m.scan(OWNER, &request, 0).unwrap();
    // The AP on 6, heard more weakly on 5 and 7.
    let frame = beacon(&AP, b"near", 6, false);
    for (channel, rssi) in [(5, -70), (6, -40), (7, -72)] {
        m.radio_mut()
            .inbound
            .push_back(heard_on(Band::Ghz2, channel, frame.clone(), rssi));
    }
    m.poll(1);
    let page = m.scan_results(OWNER, id, 0, 64, 1 << 16, 1).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(
        (page.entries[0].0.channel, page.entries[0].0.rssi_dbm),
        (6, -40)
    );
    // A flood of weaker invented BSSIDs does not push out a strong one.
    for i in 0..600u32 {
        let mut bssid = [0x02, 0, 0, 0, 0, 0];
        bssid[2..].copy_from_slice(&i.to_be_bytes());
        let spam = beacon(&bssid, b"spam", 6, false);
        m.radio_mut()
            .inbound
            .push_back(heard_on(Band::Ghz2, 6, spam, -80));
        m.poll(2);
    }
    let mut first = 0;
    let mut found = false;
    loop {
        let page = m.scan_results(OWNER, id, first, 64, 1 << 16, 3).unwrap();
        found |= page.entries.iter().any(|(e, _)| e.frame == frame);
        first += page.entries.len() as u32;
        if !page.more {
            break;
        }
    }
    assert!(found);
    assert_eq!(first as usize, crate::scan::MAX_RESULTS);
    // A frame longer than a management frame may be is not kept.
    let mut huge = beacon(&AP2, b"huge", 6, false);
    huge.resize(2000, 0);
    m.radio_mut()
        .inbound
        .push_back(heard_on(Band::Ghz2, 6, huge, -10));
    m.poll(4);
    let page = m.scan_results(OWNER, id, 0, 64, 1 << 20, 4).unwrap();
    assert!(page.entries.iter().all(|(e, _)| e.frame.len() <= 1484));
}

#[test]
fn a_full_receive_budget_asks_for_an_immediate_poll() {
    let mut m = joined(false);
    for _ in 0..(crate::rx::RX_BUDGET + 5) {
        hear(&mut m, beacon(&AP2, b"x", 6, false), 6);
    }
    m.poll(30);
    assert_eq!(m.next_deadline(), Some(0));
    m.poll(31);
    assert_ne!(m.next_deadline(), Some(0));
}

#[test]
fn a_huge_beacon_interval_is_capped() {
    let mut m = joined(false);
    let ies = IeBuilder::new().ssid(b"LazyNet").unwrap().finish();
    let fields = BeaconFields {
        timestamp: 1,
        interval: u16::MAX,
        capability: 1,
    };
    hear(&mut m, build::beacon(&AP, 1, fields, &ies), 6);
    m.poll(100);
    assert_eq!(m.next_deadline(), Some(100 + 10 * 1000));
}

#[test]
fn leaving_from_a_scan_step_deauthenticates_on_the_bss_channel() {
    let mut m = wpa2_authorized();
    let request = ScanRequest {
        channels: vec![1],
        band: 0,
        dwell_ms: 500,
        ..Default::default()
    };
    m.scan(OWNER, &request, 100).unwrap();
    m.radio_mut().sent.clear();
    m.leave(OWNER, 3).unwrap();
    let (deauth, _) = &m.radio().sent[0];
    assert_eq!(deauth[0] >> 4, SUB_DEAUTH);
    assert_eq!(m.radio().tuned.last().map(|c| c.number), Some(6));
}
