//! KNOB (CVE-2019-9506): a BR/EDR link whose encryption key was negotiated
//! below 16 octets never opens a HID channel.

use btbond::AddrKind;
use btsim::{BrPeer, SimConfig, SimController};

use super::*;

fn world_with_key_size(size: u8) -> (SimController, btsim::PeerId) {
    let mut sim = SimController::new(SimConfig {
        br_key_size: size,
        ..SimConfig::default()
    });
    let mouse = sim.add_br_peer(BrPeer::mouse(MOUSE));
    sim.br_mut(mouse).set_pairing_mode(true);
    (sim, mouse)
}

#[test]
fn a_full_length_key_is_stored_and_the_device_types() {
    let (sim, _) = world_with_key_size(16);
    let mut rig = Rig::fresh(sim, pair_with(&[MOUSE]));
    rig.run_for(2_000);
    rig.assert_clean();
    let bond = rig.host.store().find(addr(MOUSE), AddrKind::BrEdr).unwrap();
    assert_eq!(bond.key_size, 16);
    assert!(rig.host.is_typing(addr(MOUSE)));
    assert!(!rig.traced("REFUSE reason=key-size"));
}

#[test]
fn a_short_key_on_a_new_pairing_is_refused_and_leaves_no_bond() {
    let (sim, _) = world_with_key_size(7);
    let mut rig = Rig::fresh(sim, pair_with(&[MOUSE]));
    rig.run_for(2_000);
    assert!(rig.traced("REFUSE reason=key-size addr=xx:xx:xx:xx:00:02"));
    assert!(rig
        .host
        .store()
        .find(addr(MOUSE), AddrKind::BrEdr)
        .is_none());
    assert!(!rig.host.is_typing(addr(MOUSE)));
    assert!(rig.opened.is_empty(), "no input source: {:?}", rig.opened);
    assert_eq!(rig.host.link_count(), 0, "the link is dropped");
}

#[test]
fn a_short_key_on_a_reconnect_is_refused_and_keeps_the_bond() {
    let (sim, mouse) = world_with_key_size(16);
    let mut rig = Rig::fresh(sim, pair_with(&[MOUSE]));
    rig.run_for(2_000);
    assert!(rig.host.is_typing(addr(MOUSE)));
    // The same device, now behind a link that was negotiated down.
    rig.sim.set_br_key_size(1);
    let mut rig = rig.reboot(Config::default(), 10_000);
    rig.sim.wake(mouse, 10_000);
    rig.run_for(3_000);
    assert!(rig.traced("REFUSE reason=key-size"));
    assert!(!rig.host.is_typing(addr(MOUSE)));
    assert!(rig.opened.is_empty());
    assert!(rig
        .host
        .store()
        .find(addr(MOUSE), AddrKind::BrEdr)
        .is_some());
}
