//! A pairing peer that distributes the identity address of a device already
//! bonded must not take that bond over or delete it.

use btbond::AddrKind;

use super::*;
use crate::ctl_types::*;

const IMPOSTOR: [u8; 6] = [0xC0, 0xFF, 0xEE, 0x00, 0x01, 0x77];

/// A bonded LE mouse, then a reboot with a second device, in pairing mode,
/// that claims the mouse's identity. `keyboard`: the impostor is a Just
/// Works keyboard (the path that used to delete the bond).
fn bonded_then_impostor(keyboard: bool) -> (Rig, [u8; 16]) {
    let (sim, _, _) = le_world();
    let mut rig = Rig::fresh(sim, le_pair_with(&[LE_MOUSE]));
    rig.run_for(5_000);
    assert!(rig.host.is_typing(addr(LE_MOUSE)), "{:?}", rig.traces);
    let ltk = rig
        .host
        .store()
        .find(addr(LE_MOUSE), AddrKind::LePublic)
        .and_then(|b| b.ltk.as_ref())
        .map(|k| k.key)
        .expect("the mouse is bonded");

    let mut sim = SimController::new(SimConfig::default());
    let mut peer = if keyboard {
        let mut p = LePeer::keyboard(IMPOSTOR);
        p.io = btsim::IoCapability::NoInputNoOutput;
        p
    } else {
        LePeer::mouse(IMPOSTOR)
    };
    peer.claimed_identity = Some(addr(LE_MOUSE));
    let id = sim.add_le_peer(peer);
    sim.enter_pairing_mode(id, 0);
    let cfg = Config {
        trace_reports: true,
        ..Config::default()
    };
    let bytes = rig.host.store().encode().unwrap();
    let rig = Rig::boot(sim, BondStore::decode(&bytes).unwrap(), cfg, 0);
    (rig, ltk)
}

fn try_takeover(keyboard: bool) {
    let (mut rig, ltk) = bonded_then_impostor(keyboard);
    rig.run_for(300);
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(1_500);
    rig.host.pair(rig.now, addr(IMPOSTOR)).unwrap();
    rig.run_for(10_000);

    assert!(rig.traced("reason=identity-in-use"), "{:?}", rig.traces);
    assert!(rig.traced("REFUSE reason=identity-in-use"));
    let last = rig.pairings.last().copied().expect("pairing notices");
    assert_eq!(last.phase, Phase::Failed, "{:?}", rig.traces);
    assert!(!rig.host.is_typing(addr(LE_MOUSE)));
    let bond = rig
        .host
        .store()
        .find(addr(LE_MOUSE), AddrKind::LePublic)
        .expect("the other device's bond is still there");
    assert_eq!(bond.ltk.as_ref().map(|k| k.key), Some(ltk), "key untouched");
    assert_eq!(rig.host.store().iter().count(), 1);
}

#[test]
fn an_impostor_mouse_cannot_replace_a_bonded_devices_key() {
    try_takeover(false);
}

#[test]
fn an_impostor_just_works_keyboard_cannot_delete_a_bonded_device() {
    try_takeover(true);
}

#[test]
fn only_a_pairing_the_user_named_may_replace_a_bonded_identity() {
    let (sim, _, _) = le_world();
    let mut rig = Rig::fresh(sim, le_pair_with(&[LE_MOUSE]));
    rig.run_for(5_000);
    assert!(rig.host.is_typing(addr(LE_MOUSE)));
    let cfg = Config {
        trace_reports: true,
        ..Config::default()
    };
    let mut rig = rig.reboot(cfg, 20_000);
    rig.run_for(300);
    let (stranger, mouse) = (addr(IMPOSTOR), addr(LE_MOUSE));
    assert!(!rig.host.identity_may_replace(stranger, mouse));
    assert!(rig.host.identity_may_replace(mouse, mouse), "its own link");
    assert!(
        rig.host.identity_may_replace(stranger, addr([9; 6])),
        "free"
    );
    // The user re-pairs the bonded device, naming it.
    rig.host.pair(rig.now, mouse).unwrap();
    assert!(rig.host.identity_may_replace(stranger, mouse));
}
