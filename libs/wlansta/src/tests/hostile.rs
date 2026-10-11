//! Hostile and out-of-place input changes nothing; the seeded fuzz soak.

use std::vec;
use std::vec::Vec;

use ieee80211::build;
use ieee80211::Akm;

use super::ap::{Ap, AP, AP2, STA};
use super::*;

/// What must not move when a frame is refused.
fn snapshot(h: &H) -> (Status, Option<u64>, &'static str) {
    (h.sta.status().clone(), h.sta.next_deadline(), h.sta.phase_name())
}

fn refused(h: &mut H, kind: FrameKind, frame: Vec<u8>) {
    let before = snapshot(h);
    let dropped = h.sta.dropped();
    let actions = h.rx(kind, frame);
    assert!(actions.is_empty(), "{actions:?}");
    assert_eq!(snapshot(h), before);
    assert_eq!(h.sta.dropped(), dropped + 1);
}

#[test]
fn foreign_and_malformed_management_frames_change_nothing() {
    let ap = Ap::open(AP, "cafe");
    let impostor = Ap::open(AP2, "cafe");
    let mut h = H::new(vec![profile(CAFE, "cafe", Security::Open)]);
    let actions = h.begin();
    h.answer_scan(actions, &[&ap]);
    assert_eq!(h.sta.phase_name(), "auth");
    // Another BSS's success, garbage, a truncated frame, a protected frame,
    // a response addressed to someone else, an association response early,
    // a shared-key authentication, a data frame.
    refused(&mut h, FrameKind::Mgmt, impostor.auth_response(0));
    refused(&mut h, FrameKind::Mgmt, vec![0xFF; 40]);
    refused(&mut h, FrameKind::Mgmt, ap.auth_response(0)[..20].to_vec());
    let mut protected = ap.auth_response(0);
    protected[1] |= 0x40;
    refused(&mut h, FrameKind::Mgmt, protected);
    let other = build::auth(&[2, 9, 9, 9, 9, 9], &AP, &AP, 2, (0, 2, 0), &[]);
    refused(&mut h, FrameKind::Mgmt, other);
    refused(&mut h, FrameKind::Mgmt, ap.assoc_response(0, 1));
    let shared_key = build::auth(&STA, &AP, &AP, 2, (1, 2, 0), &[]);
    refused(&mut h, FrameKind::Mgmt, shared_key);
    let mut data = ap.auth_response(0);
    data[0] = 0x08;
    refused(&mut h, FrameKind::Mgmt, data);
    refused(&mut h, FrameKind::Eapol, vec![1, 2, 3]);
    // A deauthentication from another BSS does not end ours.
    refused(&mut h, FrameKind::Mgmt, impostor.deauth(1));
    // The real one goes through.
    h.rx(FrameKind::Mgmt, ap.auth_response(0));
    assert_eq!(h.sta.phase_name(), "assoc");
}

#[test]
fn forged_and_broken_eapol_changes_nothing() {
    let mut ap = Ap::wpa2(AP, "home", "correct horse battery", Akm::Psk);
    let mut h = H::new(vec![profile(HOME, "home", Security::Wpa2Psk)]);
    let pmk = ap.pmk;
    to_handshake(&mut h, &ap, pmk);
    let actions = h.rx(FrameKind::Eapol, ap.msg1());
    assert!(ap.take_msg2(&eapol_sent(&actions)));
    // Message 3 with a flipped MIC, from another address, with the wrong
    // ethertype, truncated, and a replay of message 1's counter.
    let mut good = ap.msg3(&[0x42; 16]);
    let mut bad_mic = good.clone();
    bad_mic[14 + 81] ^= 1;
    refused(&mut h, FrameKind::Eapol, bad_mic);
    let mut foreign = good.clone();
    foreign[6..12].copy_from_slice(&AP2);
    refused(&mut h, FrameKind::Eapol, foreign);
    let mut ethertype = good.clone();
    ethertype[13] = 0x00;
    refused(&mut h, FrameKind::Eapol, ethertype);
    refused(&mut h, FrameKind::Eapol, good[..60].to_vec());
    // EAPOL delivered as a management frame is not a management frame.
    refused(&mut h, FrameKind::Mgmt, good.clone());
    // The genuine message 3 still works.
    let actions = h.rx(FrameKind::Eapol, core::mem::take(&mut good));
    assert_eq!(h.state().0, ConnState::Connected, "{actions:?}");
    // A retransmitted message 3 is answered and installs nothing (KRACK).
    let actions = h.rx(FrameKind::Eapol, ap.msg3(&[0x42; 16]));
    assert!(actions.iter().all(|a| !matches!(a, Action::SetKey { .. })), "{actions:?}");
}

#[test]
fn frames_with_nothing_joined_are_dropped() {
    let ap = Ap::open(AP, "cafe");
    let mut h = H::new(vec![]);
    refused(&mut h, FrameKind::Mgmt, ap.auth_response(0));
    refused(&mut h, FrameKind::Mgmt, ap.deauth(1));
    assert!(h.input(Event::BeaconLoss).is_empty());
    assert!(h.input(Event::Deauthenticated { reason: 1 }).is_empty());
    assert_eq!(h.state(), (ConnState::Idle, FailReason::None));
}

#[test]
fn hostile_scan_entries_are_dropped_and_names_sanitised() {
    let mut h = H::new(vec![]);
    let actions = h.sta.scan(h.now, vec![]).unwrap();
    let mut evil = Ap::open(AP, "x");
    evil.ssid = b"\x1b[31mfree\xe2\x80\xaewifi\x07\xff".to_vec();
    let mut entries = vec![evil.entry()];
    entries.push(ScanEntry {
        frame: vec![0x80; 10],
        channel: 1,
        band: 0,
        rssi_dbm: -1,
        age_ms: 0,
    });
    entries.push(ScanEntry {
        band: 99,
        ..evil.entry()
    });
    entries.push(ScanEntry {
        channel: 300,
        ..evil.entry()
    });
    h.input(Event::ScanStarted { scan_id: 1 });
    let _ = actions;
    h.input(Event::ScanDone {
        scan_id: 1,
        aborted: false,
        entries,
    });
    assert_eq!(h.sta.dropped(), 3);
    let listed = h.sta.networks(crate::Viewer::User(1000));
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].ssid, "[31mfreewifi\u{FFFD}");
}

#[test]
fn seeded_fuzz_over_radio_events() {
    let (mut runs, mut connected) = (0, 0);
    fuzzkit::for_seeds("wlansta::seeded_fuzz_over_radio_events", |seed, rng| {
        // Half the cases are random bytes; half are the open join with
        // random events after it and a few bits flipped, which keeps the
        // deep states (connected, rekeys, link loss) in reach.
        let script = if seed & 1 == 0 {
            let mut script = vec![0u8; rng.range(0, 4096) as usize];
            rng.fill(&mut script);
            script
        } else {
            let mut script = crate::fuzz::open_join_script();
            let len = rng.range(0, 512) as usize;
            let tail = rng.bytes(len);
            script.extend_from_slice(&tail);
            let flips = rng.range(0, 3) as usize;
            rng.flip_bits(&mut script, flips);
            script
        };
        runs += 1;
        if crate::fuzz::run(&script) == ConnState::Connected {
            connected += 1;
        }
    });
    // A replay (`FUZZ_SEED`) runs one case; a soak must reach the deep states.
    assert!(runs < 64 || connected > 0, "the fuzz never reached a connection");
}

#[test]
fn fuzz_reaches_connected_with_a_valid_script() {
    // The structured seed: the fuzz grammar can drive a full open join, so
    // random mutations of it explore the deep states.
    let seed = crate::fuzz::open_join_script();
    assert_eq!(crate::fuzz::run(&seed), ConnState::Connected);
}
