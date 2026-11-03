//! The control surface (`os.lazy.bt.v1`): discovery, pairing driven by a
//! caller, numbers to compare, forgetting, and the refusals around them.

use btsim::{BrPeer, IoCapability, PeerId, SimConfig, SimController};

use super::*;
use crate::ctl_types::*;
use crate::UI_WINDOW_MS;

pub(super) fn kbd_addr() -> BdAddr {
    addr(KBD)
}

/// A host with no pairing policy at all (a normal build) next to a BR/EDR
/// keyboard and mouse; the keyboard is in pairing mode.
pub(super) fn br_rig() -> (Rig, PeerId, PeerId) {
    let (mut sim, kbd, mouse) = world();
    sim.enter_pairing_mode(kbd, 0);
    (Rig::fresh(sim, Config::default()), kbd, mouse)
}

fn le_rig() -> (Rig, PeerId, PeerId) {
    let (sim, kbd, mouse) = le_world();
    // `trace=1` is what tells the simulated keyboard the passkey shown.
    let cfg = Config {
        trace_reports: true,
        ..Config::default()
    };
    (Rig::fresh(sim, cfg), kbd, mouse)
}

fn phases(rig: &Rig) -> Vec<Phase> {
    rig.pairings.iter().map(|p| p.phase).collect()
}

fn names(rig: &Rig) -> Vec<String> {
    rig.host.devices().into_iter().map(|d| d.name).collect()
}

#[test]
fn br_discovery_lists_the_device_in_pairing_mode_only() {
    let (mut rig, _, _) = br_rig();
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(3_000);
    let devices = rig.host.devices();
    assert_eq!(devices.len(), 1, "{devices:?}");
    let d = &devices[0];
    assert_eq!(d.addr, kbd_addr());
    assert_eq!(d.name, "Sim Keyboard", "the name came in the EIR");
    assert_eq!(
        (d.transport, d.input),
        (Transport::BrEdr, InputKind::Keyboard)
    );
    assert!(d.rssi.is_some() && !d.bonded && !d.connected);
    assert_eq!(rig.device_events.len(), 1);
    assert_eq!(rig.device_events[0].0, DeviceChange::Found);
    rig.assert_clean();
}

#[test]
fn br_names_are_requested_when_the_results_carry_none() {
    let mut sim = SimController::new(SimConfig {
        extended_inquiry: false,
        ..SimConfig::default()
    });
    let kbd = sim.add_br_peer(BrPeer::keyboard(KBD));
    sim.enter_pairing_mode(kbd, 0);
    let mut rig = Rig::fresh(sim, Config::default());
    rig.host.start_discovery(rig.now, 8).unwrap();
    rig.run_for(4_000);
    assert_eq!(names(&rig), ["Sim Keyboard"], "{:?}", rig.device_events);
    // Found without a name, then Changed with it.
    let kinds: Vec<_> = rig.device_events.iter().map(|e| e.0).collect();
    assert_eq!(kinds, [DeviceChange::Found, DeviceChange::Changed]);
    rig.assert_clean();
}

#[test]
fn discovery_ends_by_itself_and_on_request() {
    let (mut rig, _, _) = br_rig();
    rig.host.start_discovery(rig.now, 2).unwrap();
    assert!(rig.host.is_discovering());
    rig.run_for(5_000);
    assert!(!rig.host.is_discovering());
    assert!(!rig.sim.commands_seen().is_empty());
    rig.host.start_discovery(rig.now, 30).unwrap();
    rig.run_for(500);
    rig.host.stop_discovery(rig.now);
    rig.run_for(500);
    assert!(!rig.host.is_discovering());
    // The cancelled inquiry leaves the radio free for a page.
    rig.host.pair(rig.now, kbd_addr()).unwrap();
    rig.run_for(30_000);
    rig.assert_clean();
    assert_eq!(
        rig.host.pairing_state().phase,
        Phase::Paired,
        "{:?}",
        rig.pairings
    );
}

#[test]
fn discovery_never_admits_a_device() {
    let (mut rig, kbd, _) = br_rig();
    rig.host.start_discovery(rig.now, 20).unwrap();
    rig.run_for(2_000);
    // The listed keyboard pages the host: nobody asked to pair with it.
    rig.sim.wake(kbd, rig.now);
    rig.run_for(3_000);
    assert!(rig.host.devices().iter().all(|d| !d.bonded));
    assert!(rig.sources.is_empty() && rig.host.store().iter().count() == 0);
    rig.assert_clean();
}

#[test]
fn a_caller_pairs_a_br_keyboard_with_a_passkey() {
    let (mut rig, kbd, _) = br_rig();
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(2_000);
    assert_eq!(rig.sim.scan_enable(), 2, "page scan only: not discoverable");
    rig.host.pair(rig.now, kbd_addr()).unwrap();
    rig.run_for(100);
    assert_eq!(rig.sim.scan_enable(), 3, "discoverable while pairing");
    rig.run_for(15_000);
    rig.assert_clean();
    let phases = phases(&rig);
    assert_eq!(phases.first(), Some(&Phase::Connecting));
    assert!(phases.contains(&Phase::ShowPasskey), "{phases:?}");
    assert_eq!(phases.last(), Some(&Phase::Paired));
    let shown = rig
        .pairings
        .iter()
        .find(|p| p.phase == Phase::ShowPasskey)
        .unwrap();
    assert!(shown.passkey < 1_000_000 && shown.addr == Some(kbd_addr()));
    // The passkey is not in any trace of a normal build.
    assert!(!rig.traced("PASSKEY"));
    assert_eq!(rig.sim.scan_enable(), 2, "the window closed");
    assert!(rig.host.is_typing(kbd_addr()), "{:#?}", rig.traces);
    let d = rig
        .host
        .devices()
        .into_iter()
        .find(|d| d.addr == kbd_addr())
        .unwrap();
    assert!(d.bonded && d.connected && d.input == InputKind::Keyboard);
    assert!(rig.sim.bond_of(kbd).is_some());
    rig.sim.press_key(kbd, rig.now, KEY_A);
    rig.run_for(500);
    assert_eq!(rig.typed(), [u16::from(KEY_A)]);
}

#[test]
fn a_br_mouse_pairs_without_a_number() {
    let (mut sim, _, mouse) = world();
    sim.enter_pairing_mode(mouse, 0);
    let mut rig = Rig::fresh(sim, Config::default());
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(2_000);
    rig.host.pair(rig.now, addr(MOUSE)).unwrap();
    rig.run_for(15_000);
    rig.assert_clean();
    assert_eq!(phases(&rig), [Phase::Connecting, Phase::Paired]);
    assert!(rig.host.is_typing(addr(MOUSE)));
}

#[test]
fn numeric_comparison_waits_for_the_caller_and_accept_pairs() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).io_capability = 1;
    sim.enter_pairing_mode(kbd, 0);
    let mut rig = Rig::fresh(sim, Config::default());
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(2_000);
    rig.host.pair(rig.now, kbd_addr()).unwrap();
    rig.run_for(10_000);
    let state = rig.host.pairing_state();
    assert_eq!(state.phase, Phase::ConfirmNumber, "{:?}", rig.pairings);
    assert!(state.number < 1_000_000);
    // Nothing moves until the caller answers; the wrong device is refused.
    assert_eq!(
        rig.host.confirm_number(rig.now, addr(MOUSE), true),
        Err(CtlError::Invalid)
    );
    rig.host.confirm_number(rig.now, kbd_addr(), true).unwrap();
    rig.run_for(10_000);
    rig.assert_clean();
    assert_eq!(rig.host.pairing_state().phase, Phase::Paired);
    let bond = rig.host.store().iter().next().unwrap();
    assert!(
        bond.authenticated,
        "Numeric Comparison is an authenticated key"
    );
    assert!(rig.host.is_typing(kbd_addr()));
}

#[test]
fn numeric_comparison_rejected_makes_no_bond() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).io_capability = 1;
    sim.enter_pairing_mode(kbd, 0);
    let mut rig = Rig::fresh(sim, Config::default());
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(2_000);
    rig.host.pair(rig.now, kbd_addr()).unwrap();
    rig.run_for(10_000);
    rig.host.confirm_number(rig.now, kbd_addr(), false).unwrap();
    rig.run_for(5_000);
    let state = rig.host.pairing_state();
    assert_eq!(
        (state.phase, state.reason),
        (Phase::Failed, FailReason::Rejected)
    );
    assert_eq!(rig.host.store().iter().count(), 0);
    assert_eq!(rig.sim.scan_enable(), 2, "the window closed");
}

#[test]
fn an_unreachable_device_times_out_and_closes_the_window() {
    let (mut rig, kbd, _) = br_rig();
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(2_000);
    // The user lets go of the pairing button: the device stops answering.
    rig.sim.br_mut(kbd).pairing_mode = false;
    rig.host.pair(rig.now, kbd_addr()).unwrap();
    rig.run_for(UI_WINDOW_MS);
    let state = rig.host.pairing_state();
    assert_eq!(
        (state.phase, state.reason),
        (Phase::Failed, FailReason::Timeout)
    );
    assert_eq!(rig.sim.scan_enable(), 2);
    assert_eq!(rig.host.store().iter().count(), 0);
}

#[test]
fn cancelling_closes_the_window_and_a_second_pair_is_busy() {
    let (mut rig, _, _) = br_rig();
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(2_000);
    rig.host.pair(rig.now, kbd_addr()).unwrap();
    assert_eq!(rig.host.pair(rig.now, kbd_addr()), Err(CtlError::Busy));
    assert_eq!(rig.host.start_discovery(rig.now, 5), Err(CtlError::Busy));
    rig.run_for(300);
    rig.host.cancel_pairing(rig.now);
    rig.run_for(2_000);
    let state = rig.host.pairing_state();
    assert_eq!(
        (state.phase, state.reason),
        (Phase::Failed, FailReason::Cancelled)
    );
    assert_eq!(rig.sim.scan_enable(), 2);
    assert_eq!(rig.host.store().iter().count(), 0);
    assert_eq!(rig.host.link_count(), 0, "the half-made link is dropped");
    // A new pairing may start.
    assert_eq!(rig.host.pair(rig.now, kbd_addr()), Ok(()));
    // Cancelling when nothing runs is fine.
    rig.host.cancel_pairing(rig.now);
    rig.host.cancel_pairing(rig.now);
}

#[test]
fn pair_refuses_what_it_cannot_do() {
    let (mut rig, _, _) = br_rig();
    assert_eq!(
        rig.host.pair(rig.now, addr([9; 6])),
        Err(CtlError::NotFound)
    );
    assert_eq!(
        rig.host.confirm_number(rig.now, kbd_addr(), true),
        Err(CtlError::Invalid)
    );
    assert_eq!(
        rig.host.forget_device(rig.now, addr([9; 6])),
        Err(CtlError::NotFound)
    );
    // Before the controller is up there is nothing to pair with.
    let (sim, _, _) = world();
    let mut host = Host::new(Config::default(), BondStore::new(), 0);
    assert_eq!(host.pair(0, kbd_addr()), Err(CtlError::NoController));
    assert_eq!(host.start_discovery(0, 5), Err(CtlError::NoController));
    drop(sim);
}

#[test]
fn forget_removes_the_bond_the_link_and_the_source() {
    let (mut rig, kbd, _) = br_rig();
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(2_000);
    rig.host.pair(rig.now, kbd_addr()).unwrap();
    rig.run_for(15_000);
    assert!(rig.host.is_typing(kbd_addr()));
    rig.device_events.clear();
    rig.host.forget_device(rig.now, kbd_addr()).unwrap();
    rig.run_for(1_000);
    assert!(!rig.host.is_typing(kbd_addr()));
    assert_eq!(rig.host.store().iter().count(), 0);
    assert!(rig.sources.is_empty());
    assert_eq!(rig.host.link_count(), 0);
    assert!(rig
        .device_events
        .iter()
        .any(|(c, d)| *c == DeviceChange::Removed && d.addr == kbd_addr()));
    // The saved file no longer holds the bond either.
    let saved = BondStore::decode(rig.saved.as_ref().unwrap()).unwrap();
    assert_eq!(saved.iter().count(), 0);
    // It is no longer a bond, so a device that pages anyway is refused.
    rig.sim.br_mut(kbd).link_key = None;
    rig.sim.wake(kbd, rig.now);
    rig.run_for(2_000);
    assert!(!rig.host.is_typing(kbd_addr()));
}

#[test]
fn a_bonded_device_is_listed_with_its_state_and_no_key() {
    let (mut rig, _, _) = br_rig();
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(2_000);
    rig.host.pair(rig.now, kbd_addr()).unwrap();
    rig.run_for(15_000);
    let rig = rig.reboot(Config::default(), 20_000);
    let mut rig = rig;
    rig.run_for(500);
    let devices = rig.host.devices();
    assert_eq!(devices.len(), 1);
    assert!(devices[0].bonded && !devices[0].connected);
    assert_eq!(devices[0].addr, kbd_addr());
    assert!(!devices[0].name.is_empty());
}

#[test]
fn le_discovery_lists_hid_advertisers() {
    let (mut rig, _, _) = le_rig();
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(2_000);
    let devices = rig.host.devices();
    let le: Vec<_> = devices
        .iter()
        .filter(|d| d.transport == Transport::Le)
        .collect();
    assert!(
        le.iter()
            .any(|d| d.addr == addr(LE_KBD) && d.input == InputKind::Keyboard),
        "{devices:?}"
    );
    assert!(
        le.iter()
            .any(|d| d.addr == addr(LE_MOUSE) && d.input == InputKind::Pointer),
        "{devices:?}"
    );
    assert!(devices.iter().all(|d| !d.bonded));
    // Listed is not admitted.
    assert!(rig.sources.is_empty() && rig.host.link_count() == 0);
    rig.host.stop_discovery(rig.now);
    rig.run_for(500);
    rig.assert_clean();
}

#[test]
fn a_caller_pairs_an_le_keyboard_with_a_passkey() {
    let (mut rig, kbd, _) = le_rig();
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(1_500);
    rig.host.pair(rig.now, addr(LE_KBD)).unwrap();
    rig.run_for(10_000);
    rig.assert_clean();
    let phases = phases(&rig);
    assert!(
        phases.contains(&Phase::ShowPasskey),
        "{phases:?} {:#?}",
        rig.traces
    );
    assert_eq!(phases.last(), Some(&Phase::Paired));
    assert!(rig.host.is_typing(addr(LE_KBD)));
    assert!(rig.host.store().iter().next().unwrap().authenticated);
    rig.sim.press_key(kbd, rig.now, KEY_A);
    rig.run_for(500);
    assert_eq!(rig.typed(), [u16::from(KEY_A)]);
    // The other advertiser was not paired: only the named device is.
    assert_eq!(rig.host.store().iter().count(), 1);
}

#[test]
fn an_le_mouse_pairs_and_a_just_works_keyboard_is_refused_with_a_reason() {
    let (mut sim, kbd, mouse) = le_world();
    sim.le_mut(kbd).io = IoCapability::NoInputNoOutput;
    let cfg = Config::default();
    let mut rig = Rig::fresh(sim, cfg);
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(1_500);
    rig.host.pair(rig.now, addr(LE_MOUSE)).unwrap();
    rig.run_for(8_000);
    assert_eq!(
        rig.host.pairing_state().phase,
        Phase::Paired,
        "{:#?}",
        rig.traces
    );
    let _ = mouse;
    rig.host.start_discovery(rig.now, 5).unwrap();
    rig.run_for(1_500);
    rig.host.pair(rig.now, addr(LE_KBD)).unwrap();
    rig.run_for(8_000);
    let state = rig.host.pairing_state();
    assert_eq!(
        (state.phase, state.reason),
        (Phase::Failed, FailReason::Unsupported),
        "{:#?}",
        rig.traces
    );
    assert!(!rig.host.is_typing(addr(LE_KBD)));
    assert_eq!(
        rig.host.store().iter().count(),
        1,
        "the keyboard got no bond"
    );
}

#[test]
fn le_numeric_comparison_waits_for_the_caller() {
    for accept in [true, false] {
        let (mut sim, kbd, _) = le_world();
        sim.le_mut(kbd).io = IoCapability::DisplayYesNo;
        let mut rig = Rig::fresh(sim, Config::default());
        rig.host.start_discovery(rig.now, 5).unwrap();
        rig.run_for(1_500);
        rig.host.pair(rig.now, addr(LE_KBD)).unwrap();
        rig.run_for(5_000);
        let state = rig.host.pairing_state();
        assert_eq!(
            state.phase,
            Phase::ConfirmNumber,
            "{:?} {:#?}",
            rig.pairings,
            rig.traces
        );
        rig.host
            .confirm_number(rig.now, addr(LE_KBD), accept)
            .unwrap();
        rig.run_for(8_000);
        let state = rig.host.pairing_state();
        if accept {
            assert_eq!(state.phase, Phase::Paired, "{:#?}", rig.traces);
            assert!(rig.host.is_typing(addr(LE_KBD)));
        } else {
            assert_eq!(
                (state.phase, state.reason),
                (Phase::Failed, FailReason::Rejected)
            );
            assert_eq!(rig.host.store().iter().count(), 0);
        }
    }
}

#[test]
fn a_pairing_nobody_asked_for_stays_silent_and_declines_numbers() {
    // The harness's `pair=` argument pairs without a caller: no notices, and
    // a number to compare is declined (nobody can answer it).
    let (mut sim, kbd, _) = le_world();
    sim.le_mut(kbd).io = IoCapability::DisplayYesNo;
    let mut rig = Rig::fresh(sim, le_pair_with(&[LE_KBD]));
    rig.run_for(8_000);
    assert!(rig.pairings.is_empty());
    assert!(!rig.host.is_typing(addr(LE_KBD)));
    assert!(rig.traced("REFUSE reason=confirmation"));
}
