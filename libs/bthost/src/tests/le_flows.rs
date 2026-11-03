//! LE after pairing: reconnect with the stored handles and LTK (no
//! discovery), a reboot, a peer that changes its address, a peer that talks
//! before it is encrypted, and a link that goes.

use btbond::AddrKind;
use bthci::event::LeConnection;
use bthci::Event;

use super::*;

fn paired(target: [u8; 6], rpa: bool) -> (Rig, PeerId) {
    let (sim, kbd, mouse) = le_world();
    let id = if target == LE_KBD { kbd } else { mouse };
    let mut rig = Rig::fresh(sim, le_pair_with(&[target]));
    rig.run_for(4_000);
    assert!(rig.host.is_typing(addr(target)), "{:#?}", rig.traces);
    rig.sim.le_mut(id).use_rpa = rpa;
    (rig, id)
}

/// The requests the peer's GATT server saw on its current link.
fn requests(rig: &Rig, id: PeerId) -> Vec<u8> {
    rig.sim
        .le(id)
        .gatt()
        .map(|g| g.requests.clone())
        .unwrap_or_default()
}

const READ_BY_GROUP: u8 = 0x10;
const WRITE_REQ: u8 = 0x12;

#[test]
fn a_reconnect_encrypts_with_the_stored_key_and_skips_discovery() {
    let (mut rig, kbd) = paired(LE_KBD, false);
    rig.sim.drop_link(kbd, rig.now, 0x08);
    rig.run_for(200);
    assert!(!rig.host.is_typing(addr(LE_KBD)));
    assert!(rig.sources.is_empty(), "the source closed with the link");
    rig.keys.clear();
    rig.traces.clear();

    rig.sim.press_key(kbd, rig.now, KEY_A);
    rig.run_for(2_000);
    rig.assert_clean();
    assert!(rig.host.is_typing(addr(LE_KBD)), "{:#?}", rig.traces);
    assert_eq!(rig.keys, [(4, true), (4, false)]);
    assert!(!rig.traced("PAIR:START"), "no pairing on a reconnect");
    assert!(rig.traced("ENCRYPT addr="));
    let seen = requests(&rig, kbd);
    assert!(
        !seen.contains(&READ_BY_GROUP) && seen.contains(&WRITE_REQ),
        "stored handles only, CCCD writes: {seen:02x?}"
    );
    let line = rig.trace_line("RECONNECT ms=").expect("BT:RECONNECT");
    let ms: u64 = line["RECONNECT ms=".len()..]
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(ms < 500, "link up to first report: {ms} ms");
}

#[test]
fn after_a_reboot_the_bonded_mouse_is_connected_from_the_accept_list() {
    let (rig, mouse) = paired(LE_MOUSE, false);
    let mut rig = rig.reboot(Config::default(), 10_000);
    rig.sim.move_mouse(mouse, 10_000, 0, 4, 4);
    rig.run_for(2_000);
    rig.assert_clean();
    assert!(rig.host.is_typing(addr(LE_MOUSE)), "{:#?}", rig.traces);
    assert!(!rig.traced("PAIR:START"));
    // The fast path armed the controller with the bond before anything else.
    assert_eq!(rig.sim.accept_list(), [(0, addr(LE_MOUSE))]);
    let ready: u64 = rig.trace_line("READY ms=").unwrap()["READY ms=".len()..]
        .parse()
        .unwrap();
    assert!(ready - rig.started_at < 60, "ready at {ready}");
    assert!(rig
        .pointer_events
        .iter()
        .any(|e| matches!(e, HidEvent::Pointer(_))));
    assert!(rig.traced("RECONNECT ms="));
}

#[test]
fn a_keyboard_typing_during_the_boot_loses_nothing() {
    let (rig, kbd) = paired(LE_KBD, false);
    let mut rig = rig.reboot(Config::default(), 10_000);
    rig.sim.press_key(kbd, 10_000, KEY_A);
    rig.sim.press_key(kbd, 10_000, KEY_A + 1);
    rig.run_for(2_000);
    rig.assert_clean();
    assert_eq!(rig.typed(), [4, 5], "{:#?}", rig.traces);
}

#[test]
fn a_device_that_changes_its_address_is_recognised_by_its_irk() {
    let (rig, kbd) = paired(LE_KBD, true);
    let mut rig = rig.reboot(Config::default(), 10_000);
    let mut seen = Vec::new();
    for round in 0..3u64 {
        let at = 10_000 + round * 5_000;
        rig.sim.press_key(kbd, at, KEY_A + round as u8);
        rig.run_for(2_000);
        assert!(
            rig.host.is_typing(addr(LE_KBD)),
            "round {round} {:#?}",
            rig.traces
        );
        seen.push(rig.sim.le(kbd).adv_address());
        rig.sim.drop_link(kbd, rig.now, 0x13);
        rig.run_for(3_000);
        assert!(!rig.host.is_typing(addr(LE_KBD)));
    }
    rig.assert_clean();
    assert_eq!(rig.typed(), [4, 5, 6]);
    assert!(
        seen.iter().all(|a| a.0[5] & 0xC0 == 0x40),
        "resolvable private addresses: {seen:?}"
    );
    assert!(
        seen[0] != seen[1] && seen[1] != seen[2],
        "a fresh address each time"
    );
    // The bond stayed under the identity address.
    assert!(rig
        .host
        .store()
        .find(addr(LE_KBD), AddrKind::LePublic)
        .is_some());
}

#[test]
fn an_address_that_resolves_to_no_irk_is_not_connected() {
    let (rig, kbd) = paired(LE_KBD, true);
    let mut rig = rig.reboot(Config::default(), 10_000);
    // Another device's IRK makes the address: the host must not connect.
    rig.sim.le_mut(kbd).irk = [0x11; 16];
    rig.sim.press_key(kbd, 10_000, KEY_A);
    rig.run_for(3_000);
    assert_eq!(rig.host.link_count(), 0);
    assert!(rig.typed().is_empty());
    assert!(!rig.sim.le(kbd).is_connected());
}

/// The early notification is sent while the encryption procedure runs
/// (delay 0, it is held with the rest and replayed after the event) or after
/// the event (it arrives on an encrypted link nobody subscribed on): never
/// heard either way.
#[test]
fn a_notification_before_encryption_is_ignored() {
    for (delay, reason) in [(0, "att-unencrypted"), (3, "notification-unsolicited")] {
        let (rig, kbd) = paired(LE_KBD, false);
        let mut rig = rig.reboot(Config::default(), 10_000);
        rig.sim.le_mut(kbd).early_notify = true;
        rig.sim.le_mut(kbd).greet_delay_ms = delay;
        rig.sim.press_key(kbd, 10_000, KEY_A + 1);
        rig.run_for(2_000);
        rig.assert_clean();
        // The early key (usage 4) never arrived; the real one (5) did.
        assert_eq!(rig.typed(), [5], "delay {delay}: {:#?}", rig.traces);
        let line = format!("REFUSE reason={reason}");
        assert!(rig.traced(&line), "delay {delay}: {:#?}", rig.traces);
        assert!(rig.host.stats().dropped_reports >= 1);
    }
}

#[test]
fn a_notification_before_encryption_while_pairing_is_ignored_too() {
    let (mut sim, kbd, _) = le_world();
    sim.le_mut(kbd).early_notify = true;
    let mut rig = Rig::fresh(sim, le_pair_with(&[LE_KBD]));
    rig.run_for(5_000);
    assert!(rig.host.is_typing(addr(LE_KBD)), "{:#?}", rig.traces);
    assert!(
        rig.typed().is_empty(),
        "the early key never reached the source"
    );
    assert!(rig.traced("REFUSE reason=att-unencrypted"));
}

#[test]
fn losing_the_link_releases_what_the_device_held() {
    let (mut rig, mouse) = paired(LE_MOUSE, false);
    rig.sim.move_mouse(mouse, rig.now, 1, 0, 0);
    rig.run_for(200);
    rig.sim.drop_link(mouse, rig.now, 0x08);
    rig.run_for(200);
    assert!(rig.sources.is_empty());
    use usbhid::boot::MouseOut;
    use usbhid::report::Out;
    let edges: Vec<bool> = rig
        .pointer_events
        .iter()
        .filter_map(|e| match e {
            HidEvent::Pointer(Out::Mouse(MouseOut::Button { pressed, .. })) => Some(*pressed),
            _ => None,
        })
        .collect();
    assert_eq!(edges, [true, false], "the button went down and was let go");
    assert!(rig.kernel_released.is_empty());
    // And no key material lingers on a link that is gone.
    assert_eq!(rig.host.link_count(), 0);
}

#[test]
fn a_device_that_forgot_us_while_pairing_is_open_pairs_afresh() {
    let (mut rig, kbd) = paired(LE_KBD, false);
    rig.sim.drop_link(kbd, rig.now, 0x13);
    rig.run_for(200);
    // The user reset the keyboard: no bond, pairing mode again.
    rig.sim.le_enter_pairing_mode(kbd, rig.now);
    rig.traces.clear();
    rig.run_for(6_000);
    assert!(rig.traced("PAIR:STALE"), "{:#?}", rig.traces);
    assert!(rig.host.is_typing(addr(LE_KBD)), "{:#?}", rig.traces);
    assert_eq!(rig.host.store().len(), 1);
}

#[test]
fn a_bonded_device_with_no_such_key_is_disconnected_when_pairing_is_off() {
    let (rig, kbd) = paired(LE_KBD, false);
    let mut rig = rig.reboot(Config::default(), 10_000);
    // The keyboard lost its bond but advertises (as a bonded one would).
    rig.sim.le_mut(kbd).bond = Some(([9; 16], 1));
    rig.sim.press_key(kbd, 10_000, KEY_A);
    rig.run_for(3_000);
    assert!(rig.traced("ENCRYPT:FAIL"), "{:#?}", rig.traces);
    assert!(!rig.host.is_typing(addr(LE_KBD)));
    assert!(rig.typed().is_empty());
    assert_eq!(rig.host.store().len(), 1, "the bond stays: pairing is off");
}

#[test]
fn hostile_att_and_smp_on_a_live_link_change_nothing() {
    let (mut rig, kbd) = paired(LE_KBD, false);
    let handle = rig.sim.link_handle(kbd).expect("linked");
    let l2 = |cid: u16, payload: &[u8]| {
        let mut p = (payload.len() as u16).to_le_bytes().to_vec();
        p.extend(cid.to_le_bytes());
        p.extend_from_slice(payload);
        p
    };
    for payload in [
        &[0x1B, 0x2B][..],
        &[
            0x1B, 0x2B, 0x00, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
            20, 21,
        ],
        &[0x0B],
        &[0x01, 0x0A, 0, 0, 0x0A],
        &[0xFF; 30],
        &[],
    ] {
        rig.inject_pdu(handle, &l2(4, payload));
    }
    for payload in [&[0x01, 0x04][..], &[0x0C], &[0x0B, 0x0D], &[0xFF; 20]] {
        rig.inject_pdu(handle, &l2(6, payload));
    }
    rig.run_for(200);
    assert!(rig.host.is_typing(addr(LE_KBD)), "still the same device");
    assert!(rig.typed().is_empty(), "nothing typed by hostile bytes");
}

#[test]
fn an_unknown_handle_or_a_cancelled_connect_does_not_wedge_the_radio() {
    let (rig, kbd) = paired(LE_KBD, false);
    let mut rig = rig.reboot(Config::default(), 10_000);
    // A failed connection complete (a cancelled initiator, a timeout).
    let failed = LeConnection {
        status: 0x02,
        handle: 0,
        role: 0,
        peer_address_type: 0,
        peer: bthci::BdAddr::default(),
        interval: 0,
        latency: 0,
        supervision_timeout: 0,
        clock_accuracy: 0,
    };
    rig.inject_event(Event::LeConnectionComplete(failed));
    rig.run_for(100);
    // The host re-arms the auto-connect; the keyboard still gets in.
    rig.sim.press_key(kbd, rig.now, KEY_A);
    rig.run_for(2_000);
    assert_eq!(rig.typed(), [4], "{:#?}", rig.traces);
}
