//! LE pairing against `btsim`'s LE peers: Just Works for a mouse, Passkey
//! Entry for a keyboard, and the refusals (plan decision 6 and the Security
//! section): a Just Works keyboard, a wrong passkey, an unbonded device.

use btbond::AddrKind;
use btsim::PasskeyBehavior;

use super::*;
use crate::effect::SourceKind;

fn paired_mouse() -> (Rig, PeerId) {
    let (sim, _, mouse) = le_world();
    let mut rig = Rig::fresh(sim, le_pair_with(&[LE_MOUSE]));
    rig.run_for(4_000);
    (rig, mouse)
}

#[test]
fn a_just_works_mouse_pairs_and_moves_the_pointer() {
    let (mut rig, mouse) = paired_mouse();
    rig.assert_clean();
    assert!(rig.host.is_typing(addr(LE_MOUSE)), "{:#?}", rig.traces);
    assert_eq!(rig.opened, [(rig.opened[0].0, SourceKind::Pointer)]);
    assert!(rig.traced("PAIR:PASS"), "{:#?}", rig.traces);
    assert!(rig.traced("HID:MOUSE"));
    assert!(rig.traced("ENCRYPT addr="));

    let bond = rig
        .host
        .store()
        .find(addr(LE_MOUSE), AddrKind::LePublic)
        .expect("a bond under the identity address");
    assert!(!bond.authenticated, "Just Works is not authenticated");
    assert!(bond.secure_connections);
    assert_eq!(bond.key_size, 16);
    assert!(bond.ltk.is_some() && bond.irk.is_some());
    assert!(!bond.report_descriptor.is_empty());
    assert!(bond.gatt.report_map != 0 && !bond.gatt.reports.is_empty());
    assert!(rig.saved.is_some(), "the bond file was written");

    rig.sim.move_mouse(mouse, rig.now, 0, 5, -3);
    rig.run_for(200);
    assert!(
        rig.pointer_events
            .iter()
            .any(|e| matches!(e, HidEvent::Pointer(_))),
        "{:?}",
        rig.pointer_events
    );
}

#[test]
fn a_passkey_keyboard_pairs_with_an_authenticated_key_and_types() {
    let (sim, kbd, _) = le_world();
    let mut rig = Rig::fresh(sim, le_pair_with(&[LE_KBD]));
    rig.run_for(4_000);
    rig.assert_clean();
    assert!(rig.host.is_typing(addr(LE_KBD)), "{:#?}", rig.traces);
    assert!(rig.traced("PAIR:PASSKEY value="), "the passkey was shown");
    let bond = rig
        .host
        .store()
        .find(addr(LE_KBD), AddrKind::LePublic)
        .expect("bond");
    assert!(bond.authenticated, "Passkey Entry is authenticated");
    assert!(rig.traced("HID:KBD") && rig.traced("auth=1"));
    assert_eq!(rig.opened[0].1, SourceKind::Keyboard);

    rig.sim.press_key(kbd, rig.now, KEY_A);
    rig.run_for(300);
    assert_eq!(rig.typed(), [4], "{:#?}", rig.traces);
    assert_eq!(rig.keys, [(4, true), (4, false)]);
    // The long Report Map was read in pieces.
    let gatt = rig.sim.le(kbd).gatt().expect("linked");
    assert!(gatt.requests.contains(&0x0C), "a Read Blob was needed");
}

#[test]
fn a_short_server_mtu_makes_discovery_longer_not_wrong() {
    let (mut sim, kbd, _) = le_world();
    sim.le_mut(kbd).gatt_mtu = 23;
    let mut rig = Rig::fresh(sim, le_pair_with(&[LE_KBD]));
    rig.run_for(6_000);
    rig.assert_clean();
    assert!(rig.host.is_typing(addr(LE_KBD)), "{:#?}", rig.traces);
    rig.sim.press_key(kbd, rig.now, KEY_A);
    rig.run_for(300);
    assert_eq!(rig.typed(), [4]);
}

#[test]
fn a_just_works_keyboard_is_refused_and_its_bond_never_made() {
    let (mut sim, kbd, _) = le_world();
    // A keyboard that says it has no input: its key cannot be authenticated.
    sim.le_mut(kbd).io = btsmp::IoCapability::NoInputNoOutput;
    let mut rig = Rig::fresh(sim, le_pair_with(&[LE_KBD]));
    rig.run_for(8_000);
    rig.assert_clean();
    assert!(
        rig.traced("REFUSE reason=keyboard-unauthenticated"),
        "{:#?}",
        rig.traces
    );
    assert!(!rig.host.is_typing(addr(LE_KBD)));
    assert!(rig.opened.is_empty(), "no source was ever registered");
    assert!(rig.host.store().is_empty(), "no bond made");
    assert!(!rig.traced("PAIR:PASS"));
    // It keeps advertising in pairing mode; the host does not loop on it.
    let connects = rig.traces.iter().filter(|t| t.starts_with("CONN ")).count();
    assert_eq!(connects, 1, "{:#?}", rig.traces);
    rig.sim.press_key(kbd, rig.now, KEY_A);
    rig.run_for(500);
    assert!(rig.typed().is_empty());
}

#[test]
fn an_older_bond_is_forgotten_when_the_device_returns_as_a_just_works_keyboard() {
    let (sim, kbd, _) = le_world();
    let mut rig = Rig::fresh(sim, le_pair_with(&[LE_KBD]));
    rig.run_for(4_000);
    assert!(rig.host.is_typing(addr(LE_KBD)));
    assert_eq!(rig.host.store().len(), 1);
    // The keyboard is reset into a no-input mode and the user pairs it again.
    rig.sim.drop_link(kbd, rig.now, 0x13);
    rig.run_for(200);
    rig.sim.le_mut(kbd).io = btsmp::IoCapability::NoInputNoOutput;
    rig.sim.le_enter_pairing_mode(kbd, rig.now);
    rig.run_for(8_000);
    assert!(rig.traced("REFUSE reason=keyboard-unauthenticated"));
    assert!(rig.host.store().is_empty(), "the old bond is forgotten too");
    assert!(!rig.host.is_typing(addr(LE_KBD)));
}

#[test]
fn a_wrong_passkey_fails_the_pairing_and_makes_no_bond() {
    let (mut sim, kbd, _) = le_world();
    sim.le_mut(kbd).passkey = PasskeyBehavior::Wrong;
    let mut rig = Rig::fresh(sim, le_pair_with(&[LE_KBD]));
    rig.run_for(5_000);
    rig.assert_clean();
    assert!(rig.traced("PAIR:FAIL"), "{:#?}", rig.traces);
    assert!(rig.host.store().is_empty());
    assert!(!rig.host.is_typing(addr(LE_KBD)));
    assert!(rig.opened.is_empty());
}

#[test]
fn nobody_types_the_passkey_and_the_pairing_times_out() {
    let (mut sim, kbd, _) = le_world();
    sim.le_mut(kbd).passkey = PasskeyBehavior::Never;
    let mut rig = Rig::fresh(sim, le_pair_with(&[LE_KBD]));
    rig.run_for(40_000);
    assert!(rig.traced("reason=smp-timeout"), "{:#?}", rig.traces);
    assert!(rig.host.store().is_empty());
    assert_eq!(rig.host.link_count(), 0, "the link was dropped");
}

#[test]
fn a_device_outside_the_pairing_list_is_never_connected() {
    let (sim, _, _) = le_world();
    // The window names only an address nobody has.
    let mut rig = Rig::fresh(sim, le_pair_with(&[[0xC0, 0xFF, 0xEE, 0, 9, 9]]));
    rig.run_for(4_000);
    assert_eq!(rig.host.link_count(), 0);
    assert!(rig.opened.is_empty() && rig.host.store().is_empty());
    assert!(!rig.traced("CONN "));
}

#[test]
fn pair_any_takes_hid_advertisers_and_ignores_the_others() {
    let (mut sim, kbd, mouse) = le_world();
    // The mouse is a decoy that does not advertise HID.
    sim.le_mut(mouse).hid_adv = false;
    let cfg = Config {
        trace_reports: true,
        pair: PairPolicy::Any,
        ..Config::default()
    };
    let mut rig = Rig::fresh(sim, cfg);
    rig.run_for(5_000);
    assert!(rig.host.is_typing(addr(LE_KBD)), "{:#?}", rig.traces);
    assert!(!rig.host.is_typing(addr(LE_MOUSE)));
    assert!(!rig.sim.le(mouse).is_connected());
    let _ = kbd;
}

#[test]
fn with_pairing_off_an_unbonded_le_device_is_refused_at_the_door() {
    let (sim, _, _) = le_world();
    let mut rig = Rig::fresh(sim, Config::default());
    // The controller reports a link from a stranger (a buggy or hostile
    // controller; the host never initiates this connection).
    let stranger = bthci::event::LeConnection {
        status: 0,
        handle: 0x0123,
        role: 0,
        peer_address_type: 0,
        peer: addr(LE_KBD),
        interval: 0x18,
        latency: 0,
        supervision_timeout: 0x190,
        clock_accuracy: 0,
    };
    rig.inject_event(bthci::Event::LeConnectionComplete(stranger));
    assert!(rig.traced("REFUSE reason=unbonded"), "{:#?}", rig.traces);
    assert_eq!(rig.host.link_count(), 0);
    assert!(rig.opened.is_empty());
}

#[test]
fn smp_data_that_beats_the_encryption_event_is_held_and_then_believed() {
    for late in [false, true] {
        let mut sim = SimController::new(SimConfig {
            acl_beats_encryption: true,
            ..SimConfig::default()
        });
        let kbd = sim.add_le_peer(LePeer::keyboard(LE_KBD));
        sim.enter_pairing_mode(kbd, 0);
        let mut rig = Rig::fresh(sim, le_pair_with(&[LE_KBD]));
        rig.encryption_late = late;
        rig.run_for(5_000);
        rig.assert_clean();
        assert!(
            rig.host.is_typing(addr(LE_KBD)),
            "late={late} {:#?}",
            rig.traces
        );
        assert!(rig
            .host
            .store()
            .find(addr(LE_KBD), AddrKind::LePublic)
            .is_some());
    }
}
