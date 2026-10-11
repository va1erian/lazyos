//! Scanning: the plan per domain, channel stepping with dwell and probes,
//! results with ages and paging, aborts, refusals.

use std::vec;
use std::vec::Vec;

use ieee80211::frame::{Body, Mgmt, SUB_PROBE_REQ};
use ieee80211::ie::Elements;

use super::*;
use crate::scan::{DWELL_ACTIVE_MS, DWELL_PASSIVE_MS, PAGE_MAX};
use crate::{Error, Event, ScanRequest};

fn request(channels: &[u32], active: bool) -> ScanRequest {
    ScanRequest {
        channels: channels.to_vec(),
        band: u32::MAX,
        ssids: Vec::new(),
        active,
        dwell_ms: 0,
    }
}

fn numbers(m: &SoftMac<FakeRadio>) -> Vec<u8> {
    m.radio().tuned.iter().map(|c| c.number).collect()
}

fn probes(m: &SoftMac<FakeRadio>) -> Vec<Vec<u8>> {
    m.radio()
        .sent
        .iter()
        .filter(|(f, _)| f[0] >> 4 == SUB_PROBE_REQ)
        .map(|(f, _)| f.clone())
        .collect()
}

#[test]
fn active_scan_steps_channels_with_dwell_and_probes() {
    let mut m = attached();
    let id = m.scan(OWNER, &request(&[1, 6, 11], true), 1000).unwrap();
    assert!(m.radio().scanning);
    assert_eq!(numbers(&m), [1]);
    assert_eq!(probes(&m).len(), 1);
    let (probe, options) = &m.radio().sent[0];
    assert!(options.basic_rate && !options.data && !options.protected);
    let parsed = Mgmt::parse(probe).unwrap();
    assert_eq!(parsed.header.addr2, STA);
    let Body::ProbeRequest { ies } = parsed.body else {
        panic!("not a probe request")
    };
    let ies = Elements::parse(ies).unwrap();
    assert_eq!(ies.ssid, Some(&b""[..]), "a wildcard probe");
    assert_eq!(ies.ds_channel, Some(1));
    assert_eq!(m.next_deadline(), Some(1000 + u64::from(DWELL_ACTIVE_MS)));
    m.poll(1000 + u64::from(DWELL_ACTIVE_MS) - 1);
    assert_eq!(numbers(&m), [1]);
    m.poll(1050);
    m.poll(1100);
    assert_eq!(numbers(&m), [1, 6, 11]);
    assert_eq!(probes(&m).len(), 3);
    assert!(m.next_event().is_none());
    m.poll(1150);
    assert_eq!(
        m.next_event(),
        Some(Event::ScanDone {
            scan_id: id,
            aborted: false
        })
    );
    assert!(!m.radio().scanning);
    assert_eq!(m.stats().scans, 1);
}

#[test]
fn hidden_networks_are_probed_by_name() {
    let mut m = attached();
    let mut req = request(&[3], true);
    req.ssids = vec![b"hidden".to_vec(), b"other".to_vec()];
    m.scan(OWNER, &req, 0).unwrap();
    let names: Vec<Vec<u8>> = probes(&m)
        .iter()
        .map(|f| {
            let Body::ProbeRequest { ies } = Mgmt::parse(f).unwrap().body else {
                panic!()
            };
            Elements::parse(ies).unwrap().ssid.unwrap().to_vec()
        })
        .collect();
    assert_eq!(names, [b"hidden".to_vec(), b"other".to_vec()]);
}

#[test]
fn the_world_domain_only_listens_where_it_must() {
    let mut m = softmac();
    m.attach(OWNER).unwrap();
    // No country yet: an active scan may not transmit, a passive one may run.
    assert_eq!(m.scan(OWNER, &request(&[], true), 0), Err(Error::Again));
    let mut req = request(&[], false);
    req.band = Band::Ghz2 as u32;
    m.scan(OWNER, &req, 0).unwrap();
    for t in 1..=13u64 {
        m.poll(t * u64::from(DWELL_PASSIVE_MS));
    }
    assert_eq!(numbers(&m), (1..=13).collect::<Vec<u8>>());
    assert!(probes(&m).is_empty());

    // A named FCC country stops at 11 and probes; 5 GHz DFS stays passive.
    let mut m = attached();
    m.set_country(OWNER, "US").unwrap();
    assert_eq!(m.scan(OWNER, &request(&[12], true), 0), Err(Error::Invalid));
    m.scan(OWNER, &request(&[52, 36], true), 0).unwrap();
    m.poll(1000);
    assert_eq!(numbers(&m), [52, 36]);
    assert_eq!(probes(&m).len(), 1, "probed on 36 only");
    assert_eq!(m.radio().tuned[0].band, Band::Ghz5);
}

#[test]
fn results_are_raw_deduplicated_aged_and_paged() {
    let mut m = attached();
    let id = m.scan(OWNER, &request(&[1, 6], true), 0).unwrap();
    hear(&mut m, beacon(&AP, b"one", 1, false), 1);
    hear(&mut m, beacon(&AP, b"one", 1, false), 1);
    hear(&mut m, probe_response(&AP, b"one", 1), 1);
    m.poll(10);
    m.poll(50);
    hear(&mut m, beacon(&AP2, b"two", 6, true), 6);
    m.poll(60);
    // Something that is not a beacon is not a result.
    hear(&mut m, build::deauth(&STA, &AP, &AP, 1, 1), 6);
    m.poll(70);
    let page = m.scan_results(OWNER, id, 0, 10, 1 << 16, 100).unwrap();
    assert_eq!(page.entries.len(), 3, "beacon and probe response of AP, AP2");
    assert!(!page.more);
    let (first, age) = page.entries[0];
    assert_eq!(first.frame, beacon(&AP, b"one", 1, false));
    assert_eq!((first.channel, first.band, first.rssi_dbm, age), (1, Band::Ghz2, -40, 90));
    let (last, age) = page.entries[2];
    assert_eq!((last.channel, age), (6, 40));

    let page = m.scan_results(OWNER, id, 0, 1, 1 << 16, 100).unwrap();
    assert_eq!((page.entries.len(), page.more), (1, true));
    let page = m.scan_results(OWNER, id, 2, 5, 1 << 16, 100).unwrap();
    assert_eq!((page.entries.len(), page.more), (1, false));
    let page = m.scan_results(OWNER, id, 0, 10, 10, 100).unwrap();
    assert_eq!(page.entries.len(), 1, "a byte budget still advances one entry");
    assert!(m.scan_results(OWNER, id, 9, 10, 1 << 16, 100).unwrap().entries.is_empty());
    assert_eq!(
        m.scan_results(OWNER, id + 1, 0, 10, 1 << 16, 100).map(|p| p.entries.len()),
        Err(Error::NotFound)
    );
    assert_eq!(
        m.scan_results(OTHER, id, 0, 10, 1 << 16, 100).map(|p| p.entries.len()),
        Err(Error::Denied)
    );
    // The next scan starts empty.
    m.poll(1000);
    let next = m.scan(OWNER, &request(&[1], false), 2000).unwrap();
    assert_ne!(next, id);
    assert_eq!(
        m.scan_results(OWNER, id, 0, 10, 1 << 16, 2000).map(|p| p.entries.len()),
        Err(Error::NotFound)
    );
    assert!(m.scan_results(OWNER, next, 0, 10, 1 << 16, 2000).unwrap().entries.is_empty());
}

#[test]
fn a_flood_of_bssids_stays_bounded() {
    let mut m = attached();
    let id = m.scan(OWNER, &request(&[1], false), 0).unwrap();
    for i in 0..2000u32 {
        let mut bssid = [0x02, 0, 0, 0, 0, 0];
        bssid[2..].copy_from_slice(&i.to_be_bytes());
        hear(&mut m, beacon(&bssid, b"spam", 1, false), 1);
        m.poll(1);
    }
    let mut total = 0;
    let mut first = 0;
    loop {
        let page = m.scan_results(OWNER, id, first, 64, 1 << 16, 1).unwrap();
        assert!(page.entries.len() <= PAGE_MAX);
        total += page.entries.len();
        first += page.entries.len() as u32;
        if !page.more {
            break;
        }
    }
    assert_eq!(total, crate::scan::MAX_RESULTS);
}

#[test]
fn abort_and_refusals() {
    let mut m = attached();
    let id = m.scan(OWNER, &request(&[1, 6], false), 0).unwrap();
    assert_eq!(m.scan(OWNER, &request(&[1], false), 0), Err(Error::Busy));
    assert_eq!(m.abort_scan(OTHER, id, 5), Err(Error::Denied));
    m.abort_scan(OWNER, id + 7, 5).unwrap();
    assert!(m.next_event().is_none(), "another id aborts nothing");
    m.abort_scan(OWNER, id, 5).unwrap();
    assert_eq!(
        m.next_event(),
        Some(Event::ScanDone {
            scan_id: id,
            aborted: true
        })
    );
    m.abort_scan(OWNER, id, 6).unwrap();
    assert!(m.next_event().is_none());

    let long = vec![b'x'; 33];
    for bad in [
        request(&[14], false),
        request(&[300], false),
        request(&[200], false),
        ScanRequest {
            ssids: vec![long],
            ..request(&[1], true)
        },
        ScanRequest {
            ssids: vec![Vec::new(); 5],
            ..request(&[1], true)
        },
        request(&(1..=65).map(|_| 1).collect::<Vec<u32>>(), false),
        ScanRequest {
            band: Band::Ghz6 as u32,
            ..request(&[], false)
        },
    ] {
        assert_eq!(m.scan(OWNER, &bad, 10), Err(Error::Invalid), "{bad:?}");
    }
    assert_eq!(m.scan(OTHER, &request(&[1], false), 10), Err(Error::Denied));
}

#[test]
fn a_scan_while_joined_returns_to_the_bss_and_pauses_the_beacon_watch() {
    let mut m = joined(true);
    m.radio_mut().tuned.clear();
    let mut req = request(&[1, 11], false);
    req.dwell_ms = 1000;
    m.scan(OWNER, &req, 100).unwrap();
    // Off channel for two seconds: no beacon from the BSS, no loss reported,
    // and the watch starts again when the scan ends (at 2100).
    for t in [1100, 2100, 3000] {
        m.poll(t);
    }
    assert_eq!(numbers(&m), [1, 11, 6]);
    assert!(matches!(m.next_event(), Some(Event::ScanDone { .. })));
    assert!(m.next_event().is_none());
}
