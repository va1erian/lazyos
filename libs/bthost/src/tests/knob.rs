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

// ---- ordering and controllers without the command ------------------------

/// A keyboard bonded earlier, a new host and a controller whose key-size
/// answer takes `delay` ms, with the peer's connect ahead of the event.
fn reconnecting_keyboard(size: u8, delay: u64) -> (Rig, btsim::PeerId) {
    let mut sim = SimController::new(SimConfig::default());
    let kbd = sim.add_br_peer(BrPeer::keyboard(KBD));
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut rig = Rig::fresh(sim, pair_with(&[KBD]));
    rig.run_for(3_000);
    assert!(rig.host.is_typing(addr(KBD)), "{:?}", rig.traces);
    rig.sim.set_br_key_size(size);
    rig.sim.set_br_key_size_delay(delay);
    let mut rig = rig.reboot(Config::default(), 10_000);
    rig.encryption_late = true;
    rig.sim.press_key(kbd, 10_000, KEY_A);
    (rig, kbd)
}

fn answered(rig: &Rig) -> bool {
    let op = bthci::opcode::READ_ENCRYPTION_KEY_SIZE.to_le_bytes();
    rig.recorded.iter().any(|incoming| {
        matches!(incoming, btsim::HciIn::Event(b)
            if b.first() == Some(&0x0E) && b.get(3..5) == Some(&op[..]))
    })
}

#[test]
fn a_connect_ahead_of_a_slow_key_size_answer_waits_for_it() {
    let delay = crate::link::DEFER_MS + 700;
    let (mut rig, _) = reconnecting_keyboard(16, delay);
    for _ in 0..200 {
        rig.run_for(25);
        if !answered(&rig) {
            assert!(rig.opened.is_empty(), "opened before the answer");
        }
    }
    rig.assert_clean();
    assert!(answered(&rig));
    assert!(!rig.traced("REFUSE"), "{:#?}", rig.traces);
    assert!(rig.host.is_typing(addr(KBD)), "{:#?}", rig.traces);
    assert_eq!(rig.typed(), [4]);
}

#[test]
fn a_connect_ahead_of_a_short_key_size_answer_admits_nothing() {
    let delay = crate::link::DEFER_MS + 700;
    let (mut rig, _) = reconnecting_keyboard(7, delay);
    rig.run_for(5_000);
    assert!(rig.traced("REFUSE reason=key-size addr=xx:xx:xx:xx:00:01 octets=7"));
    assert!(rig.opened.is_empty(), "{:?}", rig.opened);
    assert!(!rig.host.is_typing(addr(KBD)));
    assert_eq!(rig.host.link_count(), 0);
}

#[test]
fn a_controller_without_the_command_is_refused_and_not_paged_again() {
    let (sim, mouse) = world_with_key_size(16);
    let mut rig = Rig::fresh(sim, pair_with(&[MOUSE]));
    rig.run_for(2_000);
    assert!(rig.host.is_typing(addr(MOUSE)));
    rig.sim.set_br_key_size_unknown(true);
    let mut rig = rig.reboot(Config::default(), 10_000);
    rig.sim.wake(mouse, 10_000);
    rig.run_for(30_000);
    let count = |reason: &str| rig.traces.iter().filter(|t| t.contains(reason)).count();
    let unsupported = count("REFUSE reason=key-size-unsupported");
    assert!(unsupported >= 1, "{:#?}", rig.traces);
    assert_eq!(count("REFUSE reason=key-size "), 0, "not a short key");
    assert!(rig.opened.is_empty());
    assert!(rig
        .host
        .store()
        .find(addr(MOUSE), AddrKind::BrEdr)
        .is_some());
    // The pager did not keep paging a device that is refused every time.
    assert!(unsupported <= 2, "{:#?}", rig.traces);
}

#[test]
fn a_failed_answer_with_the_wrong_handle_still_resolves_the_waiting_link() {
    let (mut rig, _) = reconnecting_keyboard(16, 5_000);
    for _ in 0..200 {
        rig.run_for(20);
        if rig.traced("ENCRYPT addr") {
            break;
        }
    }
    assert!(rig.traced("ENCRYPT addr"), "{:#?}", rig.traces);
    assert_eq!(rig.host.link_count(), 1);
    rig.inject_event(bthci::Event::CommandComplete {
        num_commands: 1,
        opcode: bthci::opcode::READ_ENCRYPTION_KEY_SIZE,
        return_params: vec![0x0C, 0, 0, 0],
    });
    assert!(
        rig.traced("REFUSE reason=key-size-unsupported"),
        "{:#?}",
        rig.traces
    );
    assert!(rig.opened.is_empty());
}

#[test]
fn a_repeat_encryption_change_on_a_live_link_holds_nothing_and_reruns_nothing() {
    let mut sim = SimController::new(SimConfig::default());
    let kbd = sim.add_br_peer(BrPeer::keyboard(KBD));
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut rig = Rig::fresh(sim, pair_with(&[KBD]));
    rig.run_for(3_000);
    assert!(rig.host.is_typing(addr(KBD)));
    let (handle, _) = rig.opened[0];
    let sources = rig.opened.len();
    // The refreshed key's size takes a while to read.
    rig.sim.set_br_key_size_delay(3_000);
    rig.inject_event(bthci::Event::EncryptionChange {
        status: 0,
        handle,
        enabled: true,
    });
    rig.keys.clear();
    rig.sim.press_key(kbd, rig.now, KEY_A);
    rig.run_for(300);
    assert_eq!(rig.typed(), [4], "input is not held: {:#?}", rig.traces);
    rig.run_for(4_000);
    rig.assert_clean();
    assert!(rig.host.is_typing(addr(KBD)));
    assert_eq!(rig.opened.len(), sources, "setup did not run again");
    assert!(!rig.traced("REFUSE"), "{:#?}", rig.traces);
}
