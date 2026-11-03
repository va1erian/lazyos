//! A freshly bonded device that opens its HID channels while the host is
//! still reading its SDP record must not be refused for an empty descriptor.

use btbond::AddrKind;
use btsim::hid::MOUSE_DESCRIPTOR;

use super::*;

#[test]
fn channels_opened_during_sdp_wait_for_the_record() {
    let (mut sim, _, mouse) = world();
    sim.br_mut(mouse).set_pairing_mode(true);
    sim.br_mut(mouse).eager_channels = true;
    // Small SDP responses stretch the exchange over several round trips.
    sim.br_mut(mouse).sdp_chunk = 24;
    let mut rig = Rig::fresh(sim, pair_with(&[MOUSE]));
    rig.run_for(3_000);
    rig.assert_clean();
    assert!(
        !rig.traced("REFUSE reason=no-input-class"),
        "{:?}",
        rig.traces
    );
    assert!(!rig.traced("REFUSE reason=not-bonded"), "{:?}", rig.traces);
    let bond = rig.host.store().find(addr(MOUSE), AddrKind::BrEdr).unwrap();
    assert_eq!(bond.report_descriptor, MOUSE_DESCRIPTOR);
    assert!(rig.host.is_typing(addr(MOUSE)), "{:?}", rig.traces);
    assert_eq!(rig.sources.len(), 1);
}

#[test]
fn a_device_that_never_finishes_sdp_leaves_no_empty_bond() {
    let (mut sim, _, mouse) = world();
    sim.br_mut(mouse).set_pairing_mode(true);
    sim.br_mut(mouse).sdp_chunk = 24;
    let mut rig = Rig::boot(sim, btbond::BondStore::new(), pair_with(&[MOUSE]), 0);
    // Let the link key be made, then take the link away before SDP ends.
    for _ in 0..200 {
        rig.run_for(1);
        if rig
            .host
            .store()
            .find(addr(MOUSE), AddrKind::BrEdr)
            .is_some()
        {
            break;
        }
    }
    assert!(rig
        .host
        .store()
        .find(addr(MOUSE), AddrKind::BrEdr)
        .is_some());
    rig.sim.drop_link(mouse, rig.now, 0x08);
    rig.run_for(500);
    assert!(
        rig.host
            .store()
            .find(addr(MOUSE), AddrKind::BrEdr)
            .is_none(),
        "a bond without a descriptor must not survive: {:?}",
        rig.traces
    );
}
