//! Pairing: the mouse with Just Works, the keyboard with Passkey Entry, what
//! is stored, and what the host will not accept.

use btbond::{AddrKind, BondStore, InputClass};
use btsim::hid::{KEYBOARD_DESCRIPTOR, MOUSE_DESCRIPTOR};
use btsim::BrPeer;

use super::*;
use crate::effect::SourceKind;

#[test]
fn bring_up_arms_fast_and_reports_the_controller() {
    let (sim, _, _) = world();
    let rig = Rig::fresh(sim, Config::default());
    rig.assert_clean();
    assert!(rig.host.is_armed());
    assert_eq!(rig.sim.scan_enable(), 2, "page scan is on");
    assert!(rig.sim.ssp_enabled());
    let ready = rig.trace_line("READY ms=").expect("BT:READY");
    let ms: u64 = ready["READY ms=".len()..].parse().unwrap();
    assert!(ms < 40, "armed {ms} ms after the host started");
    let ctrl = rig.trace_line("CTRL:PASS:hci=").expect("BT:CTRL:PASS");
    assert!(ctrl.contains("addr=**:**:**:**:EE:01"), "{ctrl}");
    assert!(!ctrl.contains("1B:DC"), "no more than two octets: {ctrl}");
}

#[test]
fn the_mouse_pairs_with_just_works_and_moves_the_pointer() {
    let (mut sim, _, mouse) = world();
    sim.br_mut(mouse).set_pairing_mode(true);
    let mut rig = Rig::fresh(sim, pair_with(&[MOUSE]));
    rig.run_for(2_000);
    rig.assert_clean();

    let bond = rig
        .host
        .store()
        .find(addr(MOUSE), AddrKind::BrEdr)
        .expect("bonded");
    assert!(!bond.authenticated, "Just Works is an unauthenticated key");
    assert_eq!(bond.link_key.as_ref().unwrap().key_type, 4);
    assert_eq!(bond.input, InputClass::Pointer);
    assert_eq!(bond.report_descriptor, MOUSE_DESCRIPTOR);
    assert_eq!(bond.class_of_device & 0xFF, 0x80);
    assert_eq!(bond.hid_flags & 1, 1, "this mouse pages the host");

    assert_eq!(rig.sources.len(), 1);
    assert_eq!(rig.sources[0].1, SourceKind::Pointer);
    assert!(rig.traced("PAIR:START addr=xx:xx:xx:xx:00:02"));
    assert!(rig.traced("PAIR:PASS addr=xx:xx:xx:xx:00:02 auth=0 class=pointer"));
    assert!(rig.traced("ENCRYPT addr=xx:xx:xx:xx:00:02"));
    assert!(rig.traced("HID:MOUSE addr=xx:xx:xx:xx:00:02"));
    assert!(rig.host.is_typing(addr(MOUSE)));

    rig.sim.move_mouse(mouse, rig.now, 1, 5, -3);
    rig.run_for(50);
    assert!(
        rig.pointer_events.len() >= 2,
        "motion and a button edge: {:?}",
        rig.pointer_events
    );
    rig.assert_clean();
}

#[test]
fn the_keyboard_pairs_with_passkey_entry_and_types() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut rig = Rig::fresh(sim, pair_with(&[KBD]));
    rig.run_for(3_000);
    rig.assert_clean();

    let bond = rig
        .host
        .store()
        .find(addr(KBD), AddrKind::BrEdr)
        .expect("bonded");
    assert!(bond.authenticated, "Passkey Entry is an authenticated key");
    assert_eq!(bond.link_key.as_ref().unwrap().key_type, 5);
    assert_eq!(bond.input, InputClass::Keyboard);
    assert_eq!(bond.report_descriptor, KEYBOARD_DESCRIPTOR);
    assert_eq!(bond.hid_flags & 1, 1);
    assert_eq!(rig.sources, [(rig.sources[0].0, SourceKind::Keyboard)]);
    assert!(rig.traced("PAIR:PASS addr=xx:xx:xx:xx:00:01 auth=1 class=keyboard"));
    assert!(rig.traced("HID:KBD addr=xx:xx:xx:xx:00:01 auth=1"));

    rig.sim.press_key(kbd, rig.now, KEY_A);
    rig.run_for(50);
    assert_eq!(rig.keys, [(4, true), (4, false)]);
    rig.assert_clean();
}

#[test]
fn the_passkey_is_traced_only_in_a_harness_build() {
    for harness in [true, false] {
        let (mut sim, kbd, _) = world();
        sim.br_mut(kbd).set_pairing_mode(true);
        let cfg = Config {
            trace_reports: harness,
            ..pair_with(&[KBD])
        };
        let mut rig = Rig::fresh(sim, cfg);
        rig.run_for(3_000);
        assert!(rig.host.is_typing(addr(KBD)));
        assert_eq!(rig.traced("PAIR:PASSKEY value="), harness);
        // Never the whole address, in any build.
        assert!(
            rig.traces.iter().all(|t| !t.contains("C0:FF:EE")),
            "{:?}",
            rig.traces
        );
    }
}

#[test]
fn a_wrongly_typed_passkey_leaves_no_bond() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    sim.br_mut(kbd).passkey = btsim::PasskeyBehavior::Wrong;
    let mut rig = Rig::fresh(sim, pair_with(&[KBD]));
    rig.run_for(5_000);
    assert!(rig.host.store().is_empty());
    assert!(rig.sources.is_empty());
    assert!(rig.traced("PAIR:FAIL"));
}

#[test]
fn a_keyboard_that_pairs_without_authentication_is_refused() {
    // A keyboard claiming NoInputNoOutput gets Just Works: an unauthenticated
    // key. Anyone in radio range could have made that pairing, so it must
    // not type (decision 6), and the bond is not kept.
    let (mut sim, _, _) = world();
    let mut liar = BrPeer::keyboard([0xC0, 0xFF, 0xEE, 0x00, 0x00, 0x09]);
    liar.io_capability = 3;
    liar.set_pairing_mode(true);
    sim.add_br_peer(liar);
    let mut rig = Rig::fresh(sim, pair_with(&[[0xC0, 0xFF, 0xEE, 0x00, 0x00, 0x09]]));
    rig.run_for(3_000);
    rig.assert_clean();
    assert!(rig.traced("REFUSE reason=keyboard-unauthenticated"));
    assert!(rig.opened.is_empty(), "no source was ever registered");
    assert!(rig.host.store().is_empty(), "the bond is not kept");
    assert_eq!(rig.host.link_count(), 0);
    assert_eq!(
        BondStore::decode(rig.saved.as_ref().unwrap())
            .unwrap()
            .len(),
        0,
        "the saved file does not hold it either"
    );
}

#[test]
fn bonds_are_saved_as_they_are_made_and_whole() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut rig = Rig::fresh(sim, pair_with(&[KBD]));
    rig.run_for(3_000);
    assert!(rig.saves >= 2, "at the key and again with the record");
    let stored = BondStore::decode(rig.saved.as_ref().unwrap()).unwrap();
    assert_eq!(&stored, rig.host.store());
    let bond = stored.find(addr(KBD), AddrKind::BrEdr).unwrap();
    assert!(!bond.report_descriptor.is_empty());
}

#[test]
fn a_long_sdp_record_is_followed_over_continuations() {
    let (mut sim, _, mouse) = world();
    sim.br_mut(mouse).set_pairing_mode(true);
    sim.br_mut(mouse).sdp_chunk = 40;
    let mut rig = Rig::fresh(sim, pair_with(&[MOUSE]));
    rig.run_for(3_000);
    rig.assert_clean();
    assert!(rig.host.is_typing(addr(MOUSE)));
    assert!(rig.sim.br(mouse).sdp_requests >= 3, "many round trips");
}

#[test]
fn nobody_pairs_when_pairing_is_off() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut rig = Rig::fresh(sim, Config::default());
    rig.run_for(5_000);
    assert!(rig.host.store().is_empty());
    assert!(rig.opened.is_empty());
    assert_eq!(rig.host.link_count(), 0);
}

#[test]
fn the_pairing_window_closes() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    let cfg = Config {
        pair_window_ms: 1_000,
        ..pair_with(&[KBD])
    };
    let mut rig = Rig::fresh(sim, cfg);
    // Never paged within the first attempts? Close the window, then page.
    rig.run_for(10_000);
    assert!(rig.host.is_typing(addr(KBD)), "paired inside the window");
}
