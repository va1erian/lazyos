use std::vec::Vec;

use super::{beacon_ies, AP, STA};
use crate::build::{self, BeaconFields};
use crate::frame::*;
use crate::{cap, Error};

const FIELDS: BeaconFields = BeaconFields {
    timestamp: 0x0102_0304_0506_0708,
    interval: 100,
    capability: cap::ESS | cap::PRIVACY,
};

#[test]
fn beacon_round_trip() {
    let ies = beacon_ies(b"LazyNet", 6);
    let frame = build::beacon(&AP, 42, FIELDS, &ies);
    let m = Mgmt::parse(&frame).unwrap();
    assert_eq!(m.header.subtype, SUB_BEACON);
    assert_eq!((m.header.addr2, m.header.addr3), (AP, AP));
    assert_eq!(m.header.seq_number(), 42);
    let Body::Beacon(b) = m.body else {
        panic!("not a beacon")
    };
    assert_eq!(
        (b.timestamp, b.interval, b.capability),
        (FIELDS.timestamp, 100, 0x11)
    );
    assert_eq!(b.ies, &ies[..]);
    assert!(m.body.elements().unwrap().unwrap().ssid.is_some());
}

#[test]
fn probe_request_and_response() {
    let req = build::probe_request(&STA, 1, &beacon_ies(b"", 0));
    let m = Mgmt::parse(&req).unwrap();
    assert!(matches!(m.body, Body::ProbeRequest { .. }));
    assert_eq!(
        (m.header.addr1, m.header.addr3),
        (crate::BROADCAST, crate::BROADCAST)
    );
    let resp = build::probe_response(&AP, &STA, 2, FIELDS, &beacon_ies(b"x", 1));
    let m = Mgmt::parse(&resp).unwrap();
    assert!(matches!(m.body, Body::ProbeResponse(_)));
    assert_eq!(m.header.addr1, STA);
}

#[test]
fn authentication_open_system() {
    let frame = build::auth(&AP, &STA, &AP, 3, (AUTH_OPEN, 1, STATUS_SUCCESS), &[]);
    let Body::Auth(a) = Mgmt::parse(&frame).unwrap().body else {
        panic!()
    };
    assert_eq!((a.algorithm, a.transaction, a.status), (0, 1, 0));
    assert!(a.ies.is_empty());
    // Too short for the three fixed fields.
    assert_eq!(Mgmt::parse(&frame[..frame.len() - 1]), Err(Error::Short));
}

#[test]
fn association_request_and_response() {
    let ies = beacon_ies(b"LazyNet", 6);
    let req = build::assoc_request(&STA, &AP, 4, 0x0431, 10, &ies);
    let Body::AssocRequest(r) = Mgmt::parse(&req).unwrap().body else {
        panic!()
    };
    assert_eq!(
        (r.capability, r.listen_interval, r.ies),
        (0x0431, 10, &ies[..])
    );
    let resp = build::assoc_response(&AP, &STA, 5, 0x0431, 0, 7, &[]);
    let Body::AssocResponse(r) = Mgmt::parse(&resp).unwrap().body else {
        panic!()
    };
    assert_eq!((r.status, r.aid(), r.aid_field), (0, 7, 0xC007));
    for end in 24..resp.len() {
        assert_eq!(Mgmt::parse(&resp[..end]), Err(Error::Short), "end {end}");
    }
}

#[test]
fn deauth_and_disassoc() {
    let d = build::deauth(&STA, &AP, &AP, 6, 15);
    assert!(matches!(
        Mgmt::parse(&d).unwrap().body,
        Body::Deauth { reason: 15 }
    ));
    let d = build::disassoc(&STA, &AP, &AP, 7, 8);
    assert!(matches!(
        Mgmt::parse(&d).unwrap().body,
        Body::Disassoc { reason: 8 }
    ));
    assert_eq!(Mgmt::parse(&d[..25]), Err(Error::Short));
}

#[test]
fn action_frames_are_recognised_and_ignored() {
    let f = build::action(&STA, &AP, &AP, 8, 3, &[1, 2, 3]);
    assert!(matches!(
        Mgmt::parse(&f).unwrap().body,
        Body::Action {
            category: 3,
            rest: [1, 2, 3]
        }
    ));
    // A category octet is the minimum.
    assert_eq!(Mgmt::parse(&f[..24]), Err(Error::Short));
    assert!(Mgmt::parse(&f).unwrap().body.elements().unwrap().is_none());
}

#[test]
fn other_management_subtypes_and_foreign_frames() {
    let mut reassoc = build::deauth(&STA, &AP, &AP, 1, 1);
    reassoc[0] = 2 << 4;
    assert!(matches!(
        Mgmt::parse(&reassoc).unwrap().body,
        Body::Other { subtype: 2 }
    ));
    let mut data = build::deauth(&STA, &AP, &AP, 1, 1);
    data[0] = 0x08; // type data
    assert_eq!(Mgmt::parse(&data), Err(Error::NotManagement));
    data[0] = 0x01; // protocol version 1
    assert_eq!(Mgmt::parse(&data), Err(Error::BadVersion));
    assert_eq!(Mgmt::parse(&[]), Err(Error::Short));
}

#[test]
fn protected_fragmented_and_ht_control() {
    let mut f = build::deauth(&STA, &AP, &AP, 1, 1);
    f[1] = 0x40;
    assert_eq!(Mgmt::parse(&f), Err(Error::Protected));
    f[1] = 0x04; // more fragments
    assert_eq!(Mgmt::parse(&f), Err(Error::Fragmented));
    f[1] = 0;
    f[22] |= 1; // fragment number 1
    assert_eq!(Mgmt::parse(&f), Err(Error::Fragmented));
    // The Order bit puts four octets of HT Control before the body.
    let mut f = build::deauth(&STA, &AP, &AP, 1, 15);
    f[1] = 0x80;
    f.splice(24..24, [0xAA; 4]);
    assert!(matches!(
        Mgmt::parse(&f).unwrap().body,
        Body::Deauth { reason: 15 }
    ));
    f.truncate(26);
    assert_eq!(Mgmt::parse(&f), Err(Error::Short));
}

#[test]
fn every_truncation_of_every_frame_is_handled() {
    let ies = beacon_ies(b"net", 6);
    let frames: Vec<Vec<u8>> = std::vec![
        build::beacon(&AP, 1, FIELDS, &ies),
        build::probe_response(&AP, &STA, 1, FIELDS, &ies),
        build::probe_request(&STA, 1, &ies),
        build::auth(&AP, &STA, &AP, 1, (0, 1, 0), &[]),
        build::assoc_request(&STA, &AP, 1, 1, 1, &ies),
        build::assoc_response(&AP, &STA, 1, 1, 0, 1, &ies),
        build::deauth(&STA, &AP, &AP, 1, 1),
        build::disassoc(&STA, &AP, &AP, 1, 1),
        build::action(&STA, &AP, &AP, 1, 1, &[]),
    ];
    for frame in &frames {
        for end in 0..=frame.len() {
            if let Ok(m) = Mgmt::parse(&frame[..end]) {
                let _ = m.body.elements();
            }
        }
    }
}
