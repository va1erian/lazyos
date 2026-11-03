//! The stale-bond reset of an LE link removes the LE bond, not a BR/EDR bond
//! that happens to carry the same address octets.

use btbond::AddrKind;

use super::*;

#[test]
fn the_stale_bond_reset_leaves_a_bredr_bond_with_the_same_address() {
    let (sim, _, mouse) = le_world();
    let mut rig = Rig::fresh(sim, le_pair_with(&[LE_MOUSE]));
    rig.run_for(5_000);
    assert!(rig.host.is_typing(addr(LE_MOUSE)), "{:?}", rig.traces);
    let le_bond = rig
        .host
        .store()
        .find(addr(LE_MOUSE), AddrKind::LePublic)
        .cloned()
        .unwrap();
    let mut br_bond = le_bond.clone();
    br_bond.kind = AddrKind::BrEdr;
    br_bond.ltk = None;
    br_bond.irk = None;
    br_bond.link_key = Some(btbond::LinkKey {
        key: [1; 16],
        key_type: 4,
    });
    // The BR/EDR bond comes first in the store.
    let mut store = BondStore::new();
    store.upsert(br_bond).unwrap();
    store.upsert(le_bond).unwrap();
    rig.sim.drop_link(mouse, rig.now, 0x13);
    rig.run_for(500);
    // The mouse forgot the bond (the user reset it) and waits to be paired.
    rig.sim.enter_pairing_mode(mouse, rig.now);
    let sim = rig.sim;
    let mut rig = Rig::boot(sim, store, le_pair_with(&[LE_MOUSE]), 20_000);
    rig.run_for(10_000);
    assert!(rig.traced("PAIR:STALE"), "{:?}", rig.traces);
    assert!(
        rig.host
            .store()
            .find(addr(LE_MOUSE), AddrKind::BrEdr)
            .is_some(),
        "the BR/EDR bond survives"
    );
}
