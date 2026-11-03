//! A caller pairs an LE device that advertises from a resolvable private
//! address and then distributes its identity address: the pairing the caller
//! asked for must be reported on the address it named.

use super::*;
use crate::ctl_types::*;

fn rpa_rig() -> Rig {
    let mut sim = SimController::new(SimConfig::default());
    let mut mouse = LePeer::mouse(LE_MOUSE);
    mouse.use_rpa = true;
    mouse.rpa_unbonded = true;
    let mouse = sim.add_le_peer(mouse);
    sim.enter_pairing_mode(mouse, 0);
    let cfg = Config {
        trace_reports: true,
        ..Config::default()
    };
    Rig::fresh(sim, cfg)
}

fn rpa_of(rig: &Rig) -> BdAddr {
    rig.host
        .devices()
        .into_iter()
        .find(|d| d.transport == Transport::Le)
        .map(|d| d.addr)
        .expect("the advertiser is listed")
}

#[test]
fn pairing_from_an_rpa_is_reported_paired_on_the_address_asked_for() {
    let mut rig = rpa_rig();
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(1_500);
    let rpa = rpa_of(&rig);
    assert_ne!(rpa, addr(LE_MOUSE), "it advertises from a private address");
    rig.host.pair(rig.now, rpa).unwrap();
    rig.run_for(10_000);
    rig.assert_clean();
    let last = rig.pairings.last().copied().expect("pairing notices");
    assert_eq!(last.phase, Phase::Paired, "{:?}", rig.traces);
    assert_eq!(
        last.addr,
        Some(rpa),
        "reported on the address the caller named"
    );
    // The bond is under the identity the device distributed.
    let bond = rig
        .host
        .store()
        .find(addr(LE_MOUSE), btbond::AddrKind::LePublic);
    assert!(bond.is_some());
    assert!(rig.host.is_typing(addr(LE_MOUSE)));
}

#[test]
fn a_refused_rpa_pairing_is_reported_failed_not_left_to_time_out() {
    // A keyboard that says NoInputNoOutput gets a Just Works key, which may
    // not type: refused after the identity is known.
    let mut sim = SimController::new(SimConfig::default());
    let mut kbd = LePeer::keyboard(LE_KBD);
    kbd.io = btsim::IoCapability::NoInputNoOutput;
    kbd.use_rpa = true;
    kbd.rpa_unbonded = true;
    let kbd = sim.add_le_peer(kbd);
    sim.enter_pairing_mode(kbd, 0);
    let mut rig = Rig::fresh(sim, Config::default());
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(1_500);
    let rpa = rpa_of(&rig);
    rig.host.pair(rig.now, rpa).unwrap();
    rig.run_for(10_000);
    let last = rig.pairings.last().copied().expect("pairing notices");
    assert_eq!(last.phase, Phase::Failed, "{:?}", rig.traces);
    assert_eq!(last.reason, FailReason::Unsupported);
    assert!(rig.host.store().iter().next().is_none());
}

#[test]
fn cancelling_after_the_identity_is_known_stops_the_pairing() {
    let mut rig = rpa_rig();
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(1_500);
    let rpa = rpa_of(&rig);
    rig.host.pair(rig.now, rpa).unwrap();
    // Step until the device has revealed its identity: the link is renamed.
    for _ in 0..10_000 {
        if rig.host.ctl.identity_alias.is_some() {
            break;
        }
        rig.run_for(1);
    }
    assert!(rig.host.ctl.identity_alias.is_some(), "{:?}", rig.traces);
    assert!(rig.host.store().iter().next().is_none(), "not bonded yet");
    rig.host.cancel_pairing(rig.now);
    rig.run_for(10_000);
    assert!(rig.host.store().iter().next().is_none(), "{:?}", rig.traces);
    assert!(!rig.host.is_typing(addr(LE_MOUSE)));
    assert_eq!(rig.host.link_count(), 0);
}
