//! What failures teach the station (BSS penalties, the back-off), the
//! channel and elements of a join, and profile identity across owners.

use std::vec;
use std::vec::Vec;

use ieee80211::frame::{Body, Mgmt};
use ieee80211::Elements;

use super::ap::{Ap, AP, AP2};
use super::*;

fn assoc_request_for(ap: &Ap) -> Vec<u8> {
    let mut h = H::new(vec![profile(CAFE, std::str::from_utf8(&ap.ssid).unwrap(), Security::Open)]);
    let actions = h.begin();
    h.answer_scan(actions, &[ap]);
    let actions = h.rx(FrameKind::Mgmt, ap.auth_response(0));
    sent(&actions).remove(0).1
}

#[test]
fn another_bss_of_the_ssid_is_tried_after_one_fails() {
    let mut near = Ap::open(AP, "cafe");
    near.rssi = -40;
    let mut far = Ap::open(AP2, "cafe");
    far.rssi = -70;
    far.channel = 11;
    let mut h = H::new(vec![profile(CAFE, "cafe", Security::Open)]);
    let actions = h.begin();
    let actions = h.answer_scan(actions, &[&near, &far]);
    assert!(matches!(actions[0], Action::Join { bssid: AP, .. }));
    // The strongest never answers.
    for _ in 0..crate::JOIN_TRIES {
        h.advance(crate::JOIN_TIMEOUT_MS);
    }
    assert_eq!(h.state(), (ConnState::Failed, FailReason::Timeout));
    // The retry goes to the other BSS, straight from the fresh table.
    let actions = h.advance(crate::BACKOFF_MIN_MS);
    assert!(matches!(actions[0], Action::Join { bssid: AP2, channel: 11, .. }), "{actions:?}");
    // Once the penalty is over the strongest is first again.
    h.rx(FrameKind::Mgmt, far.auth_response(17));
    let actions = h.advance(crate::PENALTY_MS);
    let actions = if matches!(actions.first(), Some(Action::Scan { .. })) {
        h.answer_scan(actions, &[&near, &far])
    } else {
        actions
    };
    assert!(matches!(actions[0], Action::Join { bssid: AP, .. }), "{actions:?}");
}

#[test]
fn the_back_off_resets_only_after_the_link_held() {
    let ap = Ap::open(AP, "cafe");
    let mut h = H::new(vec![profile(CAFE, "cafe", Security::Open)]);
    connect_open(&mut h, &ap);
    let rejoin = |h: &mut H| {
        let actions = h.advance(h.sta.next_deadline().unwrap() - h.now);
        assert!(matches!(actions[0], Action::Join { .. }), "{actions:?}");
        h.rx(FrameKind::Mgmt, ap.auth_response(0));
        h.rx(FrameKind::Mgmt, ap.assoc_response(0, 1));
        assert_eq!(h.state().0, ConnState::Connected);
    };
    h.input(Event::Deauthenticated { reason: 7 });
    assert_eq!(h.sta.next_deadline(), Some(h.now + crate::BACKOFF_MIN_MS));
    rejoin(&mut h);
    // A link that drops at once does not reset the back-off: it flaps.
    h.input(Event::Deauthenticated { reason: 7 });
    assert_eq!(h.sta.next_deadline(), Some(h.now + 2 * crate::BACKOFF_MIN_MS));
    rejoin(&mut h);
    h.now += crate::STABLE_MS;
    h.input(Event::Deauthenticated { reason: 7 });
    assert_eq!(h.sta.next_deadline(), Some(h.now + crate::BACKOFF_MIN_MS), "it held: reset");
}

#[test]
fn a_join_scan_that_times_out_joins_only_what_was_heard_lately() {
    let ap = Ap::open(AP, "cafe");
    let mut h = H::new(vec![]);
    let actions = h.sta.scan(h.now, vec![]).unwrap();
    h.answer_scan(actions, &[&ap]);
    h.now += crate::FRESH_MS + 1;
    let actions = h.sta.set_profiles(h.now, vec![profile(CAFE, "cafe", Security::Open)]);
    assert!(matches!(actions.as_slice(), [Action::Scan { .. }]), "a stale table is rescanned: {actions:?}");
    h.input(Event::ScanStarted { scan_id: 4 });
    let actions = h.advance(crate::SCAN_TIMEOUT_MS);
    assert!(matches!(actions.as_slice(), [Action::AbortScan { scan_id: 4 }]), "{actions:?}");
    assert_eq!(h.state().0, ConnState::Idle, "the old entry is not joined");
}

#[test]
fn the_aps_own_channel_is_joined_not_the_one_it_was_heard_on() {
    let mut ap = Ap::open(AP, "cafe");
    ap.channel = 6;
    ap.heard_on = Some(7);
    let mut h = H::new(vec![profile(CAFE, "cafe", Security::Open)]);
    let actions = h.begin();
    let actions = h.answer_scan(actions, &[&ap]);
    assert!(matches!(actions[0], Action::Join { channel: 6, band: Band::Ghz2, .. }), "{actions:?}");
    assert_eq!(h.sta.status().freq_mhz, 2437);
}

#[test]
fn the_association_request_drops_selectors_and_states_ht_and_wmm() {
    let mut ap = Ap::open(AP, "cafe");
    // Extended rates ending in the HT PHY membership selector, HT
    // Capabilities and a WMM Parameter element.
    ap.extra = [
        &[50, 5, 0x30, 0x48, 0x60, 0x6C, 0xFF][..],
        &[45, 26],
        &[0u8; 26],
        &[221, 7, 0x00, 0x50, 0xF2, 2, 1, 1, 0],
    ]
    .concat();
    let frame = assoc_request_for(&ap);
    let Body::AssocRequest(req) = Mgmt::parse(&frame).unwrap().body else {
        panic!()
    };
    let e = Elements::parse(req.ies).unwrap();
    let rates: Vec<u8> = [e.rates.unwrap(), e.ext_rates.unwrap_or_default()].concat();
    assert_eq!(rates.len(), 12);
    assert!(rates.iter().all(|r| (r & 0x7F) <= 108), "no membership selector: {rates:?}");
    let ht = e.ht_cap.expect("HT Capabilities for an HT BSS");
    assert_eq!((ht.len(), ht[0] & 0x0C, ht[0] & 0x02, ht[3]), (26, 0x0C, 0, 0xFF));
    assert!(e.wmm, "a WMM element for a WMM BSS");

    let plain = Ap::open(AP, "cafe");
    let frame = assoc_request_for(&plain);
    let Body::AssocRequest(req) = Mgmt::parse(&frame).unwrap().body else {
        panic!()
    };
    let e = Elements::parse(req.ies).unwrap();
    assert!(e.ht_cap.is_none() && !e.wmm, "a legacy BSS gets a legacy request");
}

fn owned(id: &str, ssid: &str, uid: u32) -> Profile {
    let mut p = profile(id, ssid, Security::Open);
    p.owner = Owner::User(uid);
    p
}

#[test]
fn two_users_with_one_id_each_join_their_own() {
    let cafe = Ap::open(AP, "cafe");
    let mut work = Ap::open(AP2, "work");
    work.channel = 11;
    let mut h = H::new(vec![owned(CAFE, "cafe", 1000), owned(CAFE, "work", 1001)]);
    assert_eq!(h.sta.profiles().len(), 2);
    let actions = h.sta.connect(h.now, Owner::User(1001), CAFE).unwrap();
    let actions = h.answer_scan(actions, &[&cafe, &work]);
    assert!(matches!(actions[0], Action::Join { bssid: AP2, .. }), "only 1001's network: {actions:?}");
    let status = h.sta.status();
    assert_eq!((status.id.as_str(), status.owner), (CAFE, Some(Owner::User(1001))));
    assert_eq!(h.sta.current_owner(), Some(Owner::User(1001)));
    // A user profile cannot take a system profile's id.
    let h = H::new(vec![owned(HOME, "evil", 1000), profile(HOME, "home", Security::Open)]);
    assert_eq!(h.sta.profiles().len(), 1);
    assert_eq!(h.sta.profiles()[0].owner, Owner::System);
}

#[test]
fn a_users_networks_auto_connect_while_they_are_logged_in() {
    let ap = Ap::open(AP, "cafe");
    let mut h = H::new(vec![owned(CAFE, "cafe", 1000)]);
    assert!(h.begin().is_empty(), "nobody is logged in");
    let actions = h.sta.set_active_users(h.now, vec![1001]);
    assert!(actions.is_empty());
    let actions = h.sta.set_active_users(h.now, vec![1000]);
    let actions = h.answer_scan(actions, &[&ap]);
    assert!(matches!(actions[0], Action::Join { .. }));
    h.rx(FrameKind::Mgmt, ap.auth_response(0));
    h.rx(FrameKind::Mgmt, ap.assoc_response(0, 1));
    assert_eq!(h.state().0, ConnState::Connected);
    // The owner logs out: their network is left.
    let actions = h.sta.set_active_users(h.now, vec![]);
    assert_eq!(leaves(&actions), vec![crate::reason::LEAVING]);
    assert_eq!(h.state(), (ConnState::Idle, FailReason::None));
    assert_eq!(h.sta.next_deadline(), None);
}
