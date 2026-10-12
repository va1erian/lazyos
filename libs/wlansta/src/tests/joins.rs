//! Joins that succeed: open, WPA2-PSK with both AKMs, hidden networks, the
//! best BSS of several, rekeys, and the frames the station builds.

use std::vec;

use ieee80211::frame::{Body, Mgmt};
use ieee80211::{Akm, Elements, Rsn};

use super::ap::{Ap, AP, AP2, STA};
use super::*;

#[test]
fn open_join_reaches_connected_with_the_right_frames() {
    let ap = Ap::open(AP, "cafe");
    let mut h = H::new(vec![profile(CAFE, "cafe", Security::Open)]);
    let actions = h.begin();
    assert!(matches!(actions.as_slice(), [Action::Scan { ssids }] if ssids.is_empty()));
    assert_eq!(h.state().0, ConnState::Scanning);
    let actions = h.answer_scan(actions, &[&ap]);
    let [Action::Join {
        bssid,
        channel: 6,
        band: Band::Ghz2,
    }, Action::Tx {
        kind: FrameKind::Mgmt,
        frame,
    }] = actions.as_slice()
    else {
        panic!("{actions:?}")
    };
    assert_eq!(*bssid, AP);
    let auth = Mgmt::parse(frame).unwrap();
    assert_eq!((auth.header.addr1, auth.header.addr2, auth.header.addr3), (AP, STA, AP));
    assert!(matches!(auth.body, Body::Auth(a) if a.algorithm == 0 && a.transaction == 1));
    assert_eq!(h.state().0, ConnState::Authenticating);
    assert_eq!(h.sta.status().bssid, Some(AP));
    assert_eq!(h.sta.status().freq_mhz, 2437);

    let actions = h.rx(FrameKind::Mgmt, ap.auth_response(0));
    let assoc = &sent(&actions)[0].1;
    let Body::AssocRequest(req) = Mgmt::parse(assoc).unwrap().body else {
        panic!()
    };
    let elements = Elements::parse(req.ies).unwrap();
    assert_eq!(elements.ssid, Some(&b"cafe"[..]));
    assert!(elements.rsn.is_none(), "no RSN element for an open network");
    let actions = h.rx(FrameKind::Mgmt, ap.assoc_response(0, 7));
    assert!(matches!(
        actions.as_slice(),
        [
            Action::SetState { state: StaState::Associated, aid: 7 },
            Action::SetState { state: StaState::Authorized, aid: 7 }
        ]
    ));
    let status = h.sta.status();
    assert_eq!(
        (status.state, status.ssid.as_str(), status.id.as_str()),
        (ConnState::Connected, "cafe", CAFE)
    );
    assert_eq!(h.sta.next_deadline(), None, "nothing waits once connected");
}

#[test]
fn wpa2_join_installs_keys_in_order_for_both_akms() {
    for akm in [Akm::Psk, Akm::PskSha256] {
        let mut ap = Ap::wpa2(AP, "home", "correct horse battery", akm);
        let mut h = H::new(vec![profile(HOME, "home", Security::Wpa2Psk)]);
        let actions = h.begin();
        let actions = h.answer_scan(actions, &[&ap]);
        let [Action::FetchPmk { id, owner, ssid }] = actions.as_slice() else {
            panic!("{actions:?}")
        };
        assert_eq!((id.as_str(), *owner, ssid.as_slice()), (HOME, Owner::System, &b"home"[..]));
        let pmk = ap.pmk;
        h.answer_pmk(actions, pmk);
        let actions = h.rx(FrameKind::Mgmt, ap.auth_response(0));
        // The association request states exactly the AP's AKM and CCMP.
        let assoc = &sent(&actions)[0].1;
        let Body::AssocRequest(req) = Mgmt::parse(assoc).unwrap().body else {
            panic!()
        };
        let rsn = Rsn::parse_body(Elements::parse(req.ies).unwrap().rsn.unwrap()).unwrap();
        assert_eq!(rsn.akms, vec![akm]);
        h.rx(FrameKind::Mgmt, ap.assoc_response(0, 1));
        assert_eq!(h.state().0, ConnState::Handshake);

        let actions = h.rx(FrameKind::Eapol, ap.msg1());
        assert!(ap.take_msg2(&eapol_sent(&actions)), "message 2 MIC");
        assert_eq!(h.state().0, ConnState::Handshake);
        let gtk = [0x42; 16];
        let actions = h.rx(FrameKind::Eapol, ap.msg3(&gtk));
        let [Action::Tx { kind: FrameKind::Eapol, frame: msg4 }, Action::SetKey {
            kind: KeyKind::Pairwise,
            index: 0,
            key: tk,
            addr: ptk_addr,
            ..
        }, Action::SetKey {
            kind: KeyKind::Group,
            index: 1,
            key: group,
            rsc,
            addr: gtk_addr,
        }, Action::SetState {
            state: StaState::Authorized,
            aid: 1,
        }] = actions.as_slice()
        else {
            panic!("{actions:?}")
        };
        ap.take_msg4(msg4);
        assert_eq!(*tk.expose(), ap.tk());
        assert_eq!((*group.expose(), *rsc), (gtk, [7; 8]));
        assert_eq!((*ptk_addr, *gtk_addr), (AP, AP));
        assert_eq!(h.state(), (ConnState::Connected, FailReason::None));

        // A group rekey: message 2 back, the new key installed, still up.
        let new_gtk = [0x61; 16];
        let actions = h.rx(FrameKind::Eapol, ap.group1(2, &new_gtk));
        assert!(matches!(
            actions.as_slice(),
            [Action::Tx { kind: FrameKind::Eapol, .. }, Action::SetKey { kind: KeyKind::Group, index: 2, .. }]
        ));
        assert_eq!(h.state().0, ConnState::Connected);
    }
}

#[test]
fn the_strongest_bss_of_the_highest_priority_network_wins() {
    let mut near = Ap::open(AP, "cafe");
    near.rssi = -40;
    let mut far = Ap::open(AP2, "cafe");
    far.rssi = -80;
    far.channel = 11;
    let mut h = H::new(vec![profile(CAFE, "cafe", Security::Open)]);
    let actions = h.begin();
    let actions = h.answer_scan(actions, &[&far, &near]);
    assert!(matches!(actions[0], Action::Join { bssid: AP, channel: 6, .. }));

    // Priority beats signal.
    let other = Ap::open(AP2, "work");
    let mut work = profile(HOME, "work", Security::Open);
    work.priority = 5;
    let mut h = H::new(vec![profile(CAFE, "cafe", Security::Open), work]);
    let actions = h.begin();
    let actions = h.answer_scan(actions, &[&near, &other]);
    assert!(matches!(actions[0], Action::Join { bssid: AP2, .. }), "{actions:?}");
}

#[test]
fn a_hidden_network_is_probed_for_and_joined_once_named() {
    let mut ap = Ap::open(AP, "secret");
    ap.hidden = true;
    let mut hidden = profile(CAFE, "secret", Security::Open);
    hidden.hidden = true;
    let mut h = H::new(vec![hidden]);
    let actions = h.begin();
    assert!(matches!(actions.as_slice(), [Action::Scan { ssids }] if ssids == &[b"secret".to_vec()]));
    // Only the hidden beacon is heard: nothing to join, auto-connect waits.
    let actions = h.answer_scan(actions, &[&ap]);
    assert!(actions.is_empty());
    assert_eq!(h.state().0, ConnState::Idle);
    // The probe response names it.
    ap.hidden = false;
    let actions = h.advance(crate::AUTO_SCAN_MS);
    let actions = h.answer_scan(actions, &[&ap]);
    assert!(matches!(actions[0], Action::Join { bssid: AP, .. }));
}

#[test]
fn security_must_match_the_profile_and_unsupported_is_never_joined() {
    // A WPA2 AP under an open profile's name is not joined.
    let ap = Ap::wpa2(AP, "cafe", "password1", Akm::Psk);
    let mut h = H::new(vec![profile(CAFE, "cafe", Security::Open)]);
    let actions = h.begin();
    assert!(h.answer_scan(actions, &[&ap]).is_empty());
    assert_eq!(h.state().0, ConnState::Idle);

    // An explicit connect to it says why.
    let actions = h.sta.connect(h.now, Owner::System, CAFE).unwrap();
    assert!(h.answer_scan(actions, &[&ap]).is_empty());
    assert_eq!(h.state(), (ConnState::Failed, FailReason::PolicyMismatch));

    // TKIP group cipher (mixed mode) and PMF required are unsupported, and
    // said so.
    let mut tkip = Ap::wpa2(AP, "home", "password1", Akm::Psk);
    tkip.rsn.as_mut().unwrap().group = ieee80211::Cipher::Tkip;
    let mut pmf = Ap::wpa2(AP2, "home", "password1", Akm::Psk);
    pmf.rsn.as_mut().unwrap().caps = ieee80211::RsnCaps(ieee80211::RsnCaps::MFPR | ieee80211::RsnCaps::MFPC);
    let mut h = H::new(vec![profile(HOME, "home", Security::Wpa2Psk)]);
    let first = h.begin();
    // The connect joins the auto-connect scan already running.
    assert!(h.sta.connect(h.now, Owner::System, HOME).unwrap().is_empty());
    assert!(h.answer_scan(first, &[&tkip, &pmf]).is_empty());
    assert_eq!(h.state(), (ConnState::Failed, FailReason::Unsupported));
    let listed = h.sta.networks(crate::Viewer::User(1000));
    assert!(listed.iter().all(|n| n.security == Security::Unsupported));
}

#[test]
fn a_recent_scan_is_joined_without_scanning_again() {
    let ap = Ap::open(AP, "cafe");
    let mut h = H::new(vec![profile(CAFE, "cafe", Security::Open)]);
    connect_open(&mut h, &ap);
    let actions = h.sta.disconnect(h.now);
    assert_eq!(leaves(&actions), vec![crate::reason::LEAVING]);
    assert_eq!(h.state(), (ConnState::Idle, FailReason::None));
    // Suspended: nothing happens on its own.
    assert_eq!(h.sta.next_deadline(), None);
    assert!(h.advance(crate::FRESH_MS / 2).is_empty());
    let actions = h.sta.connect(h.now, Owner::System, CAFE).unwrap();
    assert!(matches!(actions[0], Action::Join { .. }), "{actions:?}");
}

#[test]
fn status_changes_count_transitions_not_signal() {
    let ap = Ap::open(AP, "cafe");
    let mut h = H::new(vec![profile(CAFE, "cafe", Security::Open)]);
    connect_open(&mut h, &ap);
    let changes = h.sta.status().changes;
    // Scanning, Authenticating, Associating, Connected.
    assert_eq!(changes, 4);
    h.input(Event::Rx {
        kind: FrameKind::Mgmt,
        rssi_dbm: -70,
        frame: ap.auth_response(0),
    });
    assert_eq!(h.sta.status().changes, changes);
}
