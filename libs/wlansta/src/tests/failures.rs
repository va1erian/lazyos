//! Joins that fail, links that end, timers and back-off.

use std::vec;

use ieee80211::Akm;

use super::ap::{Ap, AP};
use super::*;
use crate::reason;

#[test]
fn a_wrong_passphrase_fails_once_and_is_not_retried() {
    let mut ap = Ap::wpa2(AP, "home", "correct horse battery", Akm::Psk);
    let mut h = H::new(vec![profile(HOME, "home", Security::Wpa2Psk)]);
    let wrong = lazyos_crypto::wifi::pbkdf2_sha1(b"wrong passphrase", b"home").unwrap();
    to_handshake(&mut h, &ap, wrong);
    let actions = h.rx(FrameKind::Eapol, ap.msg1());
    assert!(!ap.take_msg2(&eapol_sent(&actions)), "the AP must see a bad MIC");
    // The AP drops message 2 and retries message 1; the station answers.
    let actions = h.rx(FrameKind::Eapol, ap.msg1());
    assert!(!ap.take_msg2(&eapol_sent(&actions)));
    // Nothing verifies before the deadline.
    let actions = h.advance(crate::HANDSHAKE_TIMEOUT_MS);
    assert_eq!(leaves(&actions), vec![reason::HANDSHAKE_TIMEOUT]);
    let status = h.sta.status().clone();
    assert_eq!((status.state, status.reason), (ConnState::Failed, FailReason::BadPassphrase));
    assert_eq!((status.id.as_str(), status.ssid.as_str()), (HOME, "home"));
    // No retry storm: nothing is scheduled, nothing happens.
    assert_eq!(h.sta.next_deadline(), None);
    for _ in 0..10 {
        assert!(h.advance(crate::BACKOFF_MAX_MS).is_empty());
    }
    assert_eq!(h.state(), (ConnState::Failed, FailReason::BadPassphrase));
    // The user tries again: the block is lifted.
    let actions = h.sta.connect(h.now, HOME).unwrap();
    assert!(matches!(actions.as_slice(), [Action::Scan { .. }] | [Action::FetchPmk { .. }]), "{actions:?}");
}

#[test]
fn an_ap_deauthenticating_after_message_2_means_a_wrong_passphrase() {
    let mut ap = Ap::wpa2(AP, "home", "correct horse battery", Akm::Psk);
    let mut h = H::new(vec![profile(HOME, "home", Security::Wpa2Psk)]);
    to_handshake(&mut h, &ap, [9; 32]);
    h.rx(FrameKind::Eapol, ap.msg1());
    let actions = h.rx(FrameKind::Mgmt, ap.deauth(15));
    assert_eq!(leaves(&actions).len(), 1);
    assert_eq!(h.state(), (ConnState::Failed, FailReason::BadPassphrase));
    // Before message 2 it is only a timeout.
    let mut h = H::new(vec![profile(HOME, "home", Security::Wpa2Psk)]);
    to_handshake(&mut h, &ap, [9; 32]);
    h.advance(crate::HANDSHAKE_TIMEOUT_MS);
    assert_eq!(h.state(), (ConnState::Failed, FailReason::Timeout));
    assert!(h.sta.next_deadline().is_some(), "a timeout is retried");
}

#[test]
fn a_missing_secret_is_a_bad_passphrase_and_keyd_down_is_retried() {
    let ap = Ap::wpa2(AP, "home", "correct horse battery", Akm::Psk);
    let mut h = H::new(vec![profile(HOME, "home", Security::Wpa2Psk)]);
    let actions = h.begin();
    h.answer_scan(actions, &[&ap]);
    assert!(h.sta.pmk(h.now, HOME, Err(PmkError::Unavailable)).is_empty());
    assert_eq!(h.state(), (ConnState::Failed, FailReason::Timeout));
    let actions = h.advance(crate::BACKOFF_MIN_MS);
    assert!(matches!(actions.as_slice(), [Action::FetchPmk { .. }]), "{actions:?}");
    h.sta.pmk(h.now, HOME, Err(PmkError::Missing));
    assert_eq!(h.state(), (ConnState::Failed, FailReason::BadPassphrase));
    assert_eq!(h.sta.next_deadline(), None);
    // A PMK for another profile, or arriving late, is ignored.
    assert!(h.sta.pmk(h.now, CAFE, Ok(Key::new([1; 32]))).is_empty());
}

#[test]
fn deauthentication_mid_session_reconnects_after_back_off() {
    let mut ap = Ap::wpa2(AP, "home", "correct horse battery", Akm::Psk);
    let mut h = H::new(vec![profile(HOME, "home", Security::Wpa2Psk)]);
    connect_wpa2(&mut h, &mut ap);
    let actions = h.input(Event::Deauthenticated { reason: 7 });
    assert_eq!(leaves(&actions).len(), 1, "keys are dropped with a Leave");
    assert_eq!(h.state(), (ConnState::Failed, FailReason::Deauthenticated));
    // The same frame arriving too changes nothing more.
    assert!(h.rx(FrameKind::Mgmt, ap.deauth(7)).is_empty());
    assert_eq!(h.sta.next_deadline(), Some(h.now + crate::BACKOFF_MIN_MS));
    let actions = h.advance(crate::BACKOFF_MIN_MS);
    // The BSS was heard recently: straight to the PMK, no scan.
    assert!(matches!(actions.as_slice(), [Action::FetchPmk { .. }]), "{actions:?}");
    let pmk = ap.pmk;
    let actions = h.answer_pmk(actions, pmk);
    assert!(matches!(actions[0], Action::Join { .. }));
    assert_eq!(h.state().0, ConnState::Authenticating);
}

#[test]
fn beacon_loss_leaves_and_rescans_once_the_table_is_stale() {
    let ap = Ap::open(AP, "cafe");
    let mut h = H::new(vec![profile(CAFE, "cafe", Security::Open)]);
    connect_open(&mut h, &ap);
    h.now += crate::FRESH_MS + 1;
    let actions = h.input(Event::BeaconLoss);
    assert_eq!(leaves(&actions), vec![reason::INACTIVITY]);
    assert_eq!(h.state(), (ConnState::Failed, FailReason::BeaconLoss));
    let actions = h.advance(crate::BACKOFF_MIN_MS);
    assert!(matches!(actions.as_slice(), [Action::Scan { .. }]), "{actions:?}");
    // The AP is gone: auto-connect goes idle and looks again later.
    let actions = h.answer_scan(actions, &[]);
    assert!(actions.is_empty());
    assert_eq!(h.state(), (ConnState::Idle, FailReason::None));
    assert_eq!(h.sta.next_deadline(), Some(h.now + crate::AUTO_SCAN_MS));
}

#[test]
fn requests_are_retransmitted_then_time_out_with_growing_back_off() {
    let ap = Ap::open(AP, "cafe");
    let mut h = H::new(vec![profile(CAFE, "cafe", Security::Open)]);
    let actions = h.begin();
    h.answer_scan(actions, &[&ap]);
    for _ in 1..crate::JOIN_TRIES {
        let actions = h.advance(crate::JOIN_TIMEOUT_MS);
        assert_eq!(sent(&actions).len(), 1, "an authentication retransmission");
        assert_eq!(h.state().0, ConnState::Authenticating);
    }
    let actions = h.advance(crate::JOIN_TIMEOUT_MS);
    assert_eq!(leaves(&actions).len(), 1);
    assert_eq!(h.state(), (ConnState::Failed, FailReason::Timeout));
    // The retry: 1 s, then 2 s after the next failure.
    let actions = h.advance(crate::BACKOFF_MIN_MS);
    assert!(matches!(actions[0], Action::Join { .. }));
    h.rx(FrameKind::Mgmt, ap.auth_response(0));
    for _ in 1..crate::JOIN_TRIES {
        assert_eq!(sent(&h.advance(crate::JOIN_TIMEOUT_MS)).len(), 1, "an association retransmission");
    }
    h.advance(crate::JOIN_TIMEOUT_MS);
    assert_eq!(h.state(), (ConnState::Failed, FailReason::Timeout));
    assert_eq!(h.sta.next_deadline(), Some(h.now + 2 * crate::BACKOFF_MIN_MS));
}

#[test]
fn rejections_and_radio_errors_fail_the_attempt() {
    let ap = Ap::open(AP, "cafe");
    let mut h = H::new(vec![profile(CAFE, "cafe", Security::Open)]);
    let actions = h.begin();
    h.answer_scan(actions, &[&ap]);
    let actions = h.rx(FrameKind::Mgmt, ap.auth_response(17));
    assert_eq!(leaves(&actions).len(), 1);
    assert_eq!(h.state(), (ConnState::Failed, FailReason::Rejected));

    h.advance(crate::BACKOFF_MIN_MS);
    h.rx(FrameKind::Mgmt, ap.auth_response(0));
    h.rx(FrameKind::Mgmt, ap.assoc_response(12, 0));
    assert_eq!(h.state(), (ConnState::Failed, FailReason::Rejected));

    h.advance(2 * crate::BACKOFF_MIN_MS);
    let actions = h.input(Event::HwError {
        op: HwOp::Join,
        errno: -22,
    });
    assert_eq!(leaves(&actions).len(), 1);
    assert_eq!(h.state(), (ConnState::Failed, FailReason::Radio));

    // A scan the radio refuses fails a join scan too.
    let mut h = H::new(vec![profile(CAFE, "cafe", Security::Open)]);
    h.begin();
    h.input(Event::HwError {
        op: HwOp::Scan,
        errno: -11,
    });
    assert_eq!(h.state(), (ConnState::Failed, FailReason::Radio));
}

#[test]
fn a_scan_that_never_finishes_is_aborted() {
    let mut h = H::new(vec![profile(CAFE, "cafe", Security::Open)]);
    h.begin();
    h.input(Event::ScanStarted { scan_id: 9 });
    let actions = h.advance(crate::SCAN_TIMEOUT_MS);
    assert!(matches!(actions.as_slice(), [Action::AbortScan { scan_id: 9 }]), "{actions:?}");
    assert_eq!(h.state().0, ConnState::Idle);
    // Its late results are not believed.
    let ap = Ap::open(AP, "cafe");
    let late = h.input(Event::ScanDone {
        scan_id: 9,
        aborted: true,
        entries: vec![ap.entry()],
    });
    assert!(late.is_empty());
    assert_eq!(h.sta.scan_generation(), 0);
}

#[test]
fn forgetting_the_current_network_leaves_it() {
    let ap = Ap::open(AP, "cafe");
    let mut h = H::new(vec![profile(CAFE, "cafe", Security::Open)]);
    connect_open(&mut h, &ap);
    let actions = h.sta.set_profiles(h.now, vec![]);
    assert_eq!(leaves(&actions), vec![reason::LEAVING]);
    assert_eq!(h.state(), (ConnState::Idle, FailReason::None));
    assert_eq!(h.sta.connect(h.now, CAFE).unwrap_err(), Refused::NoProfile);
}

#[test]
fn user_scans_are_refused_while_joining() {
    let ap = Ap::open(AP, "cafe");
    let mut h = H::new(vec![profile(CAFE, "cafe", Security::Open)]);
    h.begin();
    assert_eq!(h.sta.scan(h.now, vec![]).unwrap_err(), Refused::Busy);
    let mut h = H::new(vec![]);
    let too_many = vec![b"a".to_vec(); crate::MAX_SCAN_SSIDS + 1];
    assert_eq!(h.sta.scan(h.now, too_many).unwrap_err(), Refused::Invalid);
    assert_eq!(h.sta.scan(h.now, vec![vec![b'x'; 33]]).unwrap_err(), Refused::Invalid);
    let actions = h.sta.scan(h.now, vec![]).unwrap();
    assert_eq!(h.state().0, ConnState::Idle, "a user scan is not a join");
    h.answer_scan(actions, &[&ap]);
    assert_eq!(h.sta.networks(crate::Viewer::User(1000)).len(), 1);
}
