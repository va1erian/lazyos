//! What the host will not do (docs/bluetooth-plan.md decision 6): an
//! unbonded, unencrypted or unauthenticated device does not type, whatever it
//! asks for. This is the CVE-2023-45866 class: HID connections accepted from
//! a device the user never paired.

use btbond::{AddrKind, Bond, BondStore, Gatt, InputClass, LinkKey};
use bthci::opcode::status;
use bthci::{Command, Event};
use btsim::hid::{KEYBOARD_DESCRIPTOR, MOUSE_DESCRIPTOR};

use super::*;

/// A keyboard that paged us, never paired, and still holds a key of its own.
#[test]
fn an_unbonded_device_that_pages_us_is_refused_and_never_types() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).link_key = Some(([7; 16], 5));
    let mut rig = Rig::fresh(sim, Config::default());
    rig.sim.press_key(kbd, rig.now, KEY_A);
    rig.run_for(2_000);
    rig.assert_clean();
    assert!(rig.traced("REFUSE reason=unbonded addr=xx:xx:xx:xx:00:01"));
    assert!(rig.opened.is_empty());
    assert!(rig.keys.is_empty());
    assert_eq!(rig.host.link_count(), 0);
    assert!(rig.host.store().is_empty());
    assert!(rig.host.stats().refused >= 1);
    assert!(
        rig.sim
            .commands_seen()
            .iter()
            .any(|c| matches!(c, Command::RejectConnectionRequest { .. })),
        "it was rejected at the connection request"
    );
    assert!(
        !rig.sim
            .commands_seen()
            .iter()
            .any(|c| matches!(c, Command::AcceptConnectionRequest { .. })),
        "never accepted"
    );
}

#[test]
fn a_device_outside_the_pairing_list_is_refused_while_the_window_is_open() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).link_key = Some(([7; 16], 5));
    let mut rig = Rig::fresh(sim, pair_with(&[MOUSE]));
    rig.sim.press_key(kbd, rig.now, KEY_A);
    rig.run_for(2_000);
    assert!(rig.traced("REFUSE reason=unbonded"));
    assert!(rig.opened.is_empty());
}

#[test]
fn the_window_closing_closes_the_door() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).link_key = Some(([7; 16], 5));
    let cfg = Config {
        pair_window_ms: 1_000,
        ..pair_with(&[KBD])
    };
    let mut rig = Rig::fresh(sim, cfg);
    rig.run_for(2_000);
    rig.sim.press_key(kbd, rig.now, KEY_A);
    rig.run_for(2_000);
    assert!(rig.traced("REFUSE reason=unbonded"));
    assert!(rig.opened.is_empty());
}

/// The mouse was paired; the machine that boots has lost its bond file.
#[test]
fn a_device_whose_bond_was_lost_does_not_type() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut rig = Rig::fresh(sim, pair_with(&[KBD]));
    rig.run_for(3_000);
    assert!(rig.host.is_typing(addr(KBD)));
    let mut sim = rig.sim;
    sim.power_cycle();
    let mut rig = Rig::boot(sim, BondStore::new(), Config::default(), 9_000);
    rig.sim.press_key(kbd, 9_000, KEY_A);
    rig.run_for(2_000);
    assert!(rig.traced("REFUSE reason=unbonded"));
    assert!(rig.keys.is_empty());
}

/// Build a bonded-but-tampered copy of a paired keyboard and reconnect it.
fn reconnect_with(edit: impl FnOnce(&mut Bond)) -> Rig {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut rig = Rig::fresh(sim, pair_with(&[KBD]));
    rig.run_for(3_000);
    assert!(rig.host.is_typing(addr(KBD)));
    let mut bond = rig
        .host
        .store()
        .find(addr(KBD), AddrKind::BrEdr)
        .unwrap()
        .clone();
    edit(&mut bond);
    let mut store = BondStore::new();
    store.upsert(bond).unwrap();
    let mut sim = rig.sim;
    sim.power_cycle();
    let mut rig = Rig::boot(sim, store, Config::default(), 9_000);
    rig.sim.press_key(kbd, 9_000, KEY_A);
    rig.run_for(2_000);
    rig
}

#[test]
fn a_keyboard_bond_with_an_unauthenticated_key_is_not_a_keyboard() {
    let rig = reconnect_with(|b| {
        b.authenticated = false;
        b.link_key.as_mut().unwrap().key_type = 4;
    });
    assert!(rig.traced("REFUSE reason=keyboard-unauthenticated"));
    assert!(rig.opened.is_empty());
    assert!(rig.keys.is_empty());
}

#[test]
fn a_pointer_bond_cannot_present_itself_as_a_keyboard() {
    // The class follows the bond's record, not what the device sends.
    let rig = reconnect_with(|b| b.input = InputClass::Pointer);
    assert!(rig.traced("REFUSE reason=no-input-class"));
    assert!(rig.opened.is_empty());
    assert!(rig.keys.is_empty());
}

#[test]
fn a_stale_key_never_types() {
    let rig = reconnect_with(|b| b.link_key.as_mut().unwrap().key[0] ^= 0xFF);
    assert!(rig.traced("AUTH:FAIL") || rig.host.link_count() == 0);
    assert!(rig.opened.is_empty());
    assert!(rig.keys.is_empty());
}

fn tampered_bond(input: InputClass, authenticated: bool, descriptor: &[u8]) -> Bond {
    Bond {
        addr: addr(KBD),
        kind: AddrKind::BrEdr,
        authenticated,
        secure_connections: false,
        key_size: 16,
        link_key: Some(LinkKey {
            key: [1; 16],
            key_type: if authenticated { 5 } else { 4 },
        }),
        ltk: None,
        irk: None,
        class_of_device: 0x05C0,
        input,
        hid_flags: 1,
        name: String::new(),
        report_descriptor: descriptor.to_vec(),
        gatt: Gatt::default(),
    }
}

#[test]
fn a_combo_device_with_an_unauthenticated_key_keeps_only_its_pointer() {
    let mut combo = KEYBOARD_DESCRIPTOR.to_vec();
    combo.extend_from_slice(MOUSE_DESCRIPTOR);
    let (sim, _, _) = world();
    let mut rig = Rig::fresh(sim, Config::default());
    let plan = rig
        .host
        .plan_for(&tampered_bond(InputClass::Combo, false, &combo))
        .expect("the pointer half remains");
    assert!(plan.pointer && !plan.keyboard);
    let plan = rig
        .host
        .plan_for(&tampered_bond(InputClass::Combo, true, &combo))
        .unwrap();
    assert!(plan.pointer && plan.keyboard);
    assert!(rig
        .host
        .plan_for(&tampered_bond(
            InputClass::Keyboard,
            false,
            KEYBOARD_DESCRIPTOR
        ))
        .is_none());
    assert!(rig
        .host
        .plan_for(&tampered_bond(
            InputClass::Pointer,
            true,
            KEYBOARD_DESCRIPTOR
        ))
        .is_none());
    rig.run_for(10);
}

// ---- hostile controllers and peers, injected ---------------------------------

/// A paired keyboard whose link then drops, leaving the bond; returns a rig
/// ready to be handed events.
fn bonded_rig() -> Rig {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut rig = Rig::fresh(sim, pair_with(&[KBD]));
    rig.run_for(3_000);
    rig.sim.drop_link(kbd, rig.now, 0x08);
    rig.run_for(200);
    assert_eq!(rig.host.link_count(), 0);
    rig.opened.clear();
    rig.keys.clear();
    rig
}

/// An L2CAP connection request for `psm` on the signalling channel.
fn connect_request(psm: u16) -> Vec<u8> {
    let mut sig = vec![0x02, 0x01, 0x04, 0x00];
    sig.extend_from_slice(&psm.to_le_bytes());
    sig.extend_from_slice(&0x0040u16.to_le_bytes());
    let mut pdu = (sig.len() as u16).to_le_bytes().to_vec();
    pdu.extend_from_slice(&1u16.to_le_bytes());
    pdu.extend(sig);
    pdu
}

fn connection_response(packet: &[u8]) -> Option<(u16, u16)> {
    // ACL header (4), L2CAP header (4), then code 0x03, ident, length, dcid,
    // scid, result, status.
    let body = packet.get(8..)?;
    (body.first() == Some(&0x03)).then(|| {
        let result = u16::from_le_bytes([body[8], body[9]]);
        let dcid = u16::from_le_bytes([body[4], body[5]]);
        (result, dcid)
    })
}

#[test]
fn hid_channels_are_refused_on_a_link_that_is_not_encrypted() {
    let mut rig = bonded_rig();
    let kbd = addr(KBD);
    // A bonded address connects; nothing authenticates it or encrypts the link.
    rig.inject_event(Event::ConnectionRequest {
        addr: kbd,
        class_of_device: 0x0540,
        link_type: 1,
    });
    rig.inject_event(Event::ConnectionComplete {
        status: 0,
        handle: 0x0050,
        addr: kbd,
        link_type: 1,
        encryption_enabled: false,
    });
    assert_eq!(rig.host.link_count(), 1);
    for psm in [0x11, 0x13] {
        let sent = rig.inject_pdu(0x0050, &connect_request(psm));
        let result = sent.iter().find_map(|p| connection_response(p));
        let (result, dcid) = result.expect("a connection response");
        assert_eq!(result, 3, "refused: security block (psm {psm:#x})");
        assert_eq!(dcid, 0, "no channel was made");
    }
    assert!(rig.traced("REFUSE reason=psm psm=0x0011"));
    assert!(rig.opened.is_empty());
    assert!(rig.keys.is_empty());
}

#[test]
fn input_on_a_channel_that_was_never_opened_is_dropped() {
    let mut rig = bonded_rig();
    let kbd = addr(KBD);
    rig.inject_event(Event::ConnectionRequest {
        addr: kbd,
        class_of_device: 0x0540,
        link_type: 1,
    });
    rig.inject_event(Event::ConnectionComplete {
        status: 0,
        handle: 0x0050,
        addr: kbd,
        link_type: 1,
        encryption_enabled: false,
    });
    // A data PDU on a dynamic channel id nobody opened.
    let mut pdu = vec![9, 0, 0x40, 0x00];
    pdu.extend_from_slice(&[0xA1, 1, 0, 0, 4, 0, 0, 0, 0]);
    rig.inject_pdu(0x0050, &pdu);
    assert!(rig.keys.is_empty() && rig.opened.is_empty());
}

#[test]
fn legacy_pairing_and_debug_keys_are_not_bonds() {
    let (sim, _, _) = world();
    let cfg = Config {
        pair: PairPolicy::Any,
        ..Config::default()
    };
    let mut rig = Rig::fresh(sim, cfg);
    let stranger = addr([1, 2, 3, 4, 5, 6]);
    for (n, key_type) in [0u8, 1, 2, 3].into_iter().enumerate() {
        let handle = 0x60 + n as u16;
        let stranger = addr([1, 2, 3, 4, 5, n as u8 + 1]);
        rig.inject_event(Event::ConnectionRequest {
            addr: stranger,
            class_of_device: 0x0540,
            link_type: 1,
        });
        rig.inject_event(Event::ConnectionComplete {
            status: 0,
            handle,
            addr: stranger,
            link_type: 1,
            encryption_enabled: false,
        });
        rig.inject_event(Event::LinkKeyNotification {
            addr: stranger,
            key: [9; 16],
            key_type,
        });
        assert!(
            rig.host.store().is_empty(),
            "key type {key_type} must not make a bond"
        );
    }
    assert_eq!(
        rig.traces
            .iter()
            .filter(|t| t.contains("REFUSE reason=key-type"))
            .count(),
        4
    );
    let _ = stranger;
    assert!(
        rig.sim.commands_seen().iter().any(|c| matches!(
            c,
            Command::Disconnect { reason, .. } if *reason == status::PAIRING_NOT_ALLOWED
        )),
        "{:?}",
        rig.sim.commands_seen()
    );
}

#[test]
fn a_stranger_that_connects_and_stalls_is_dropped() {
    let (sim, _, _) = world();
    let cfg = Config {
        pair: PairPolicy::Any,
        ..Config::default()
    };
    let mut rig = Rig::fresh(sim, cfg);
    let stranger = addr([1, 2, 3, 4, 5, 6]);
    rig.inject_event(Event::ConnectionRequest {
        addr: stranger,
        class_of_device: 0x0540,
        link_type: 1,
    });
    rig.inject_event(Event::ConnectionComplete {
        status: 0,
        handle: 0x0060,
        addr: stranger,
        link_type: 1,
        encryption_enabled: false,
    });
    assert_eq!(rig.host.link_count(), 1);
    rig.run_for(crate::link::SETUP_MS + 1_000);
    assert!(rig.traced("REFUSE reason=setup-timeout"));
    assert!(rig
        .sim
        .commands_seen()
        .iter()
        .any(|c| matches!(c, Command::Disconnect { handle: 0x0060, .. })));
}

#[test]
fn the_link_table_is_bounded() {
    let (sim, _, _) = world();
    let cfg = Config {
        pair: PairPolicy::Any,
        ..Config::default()
    };
    let mut rig = Rig::fresh(sim, cfg);
    for n in 0..20u8 {
        let a = addr([9, 9, 9, 9, 9, n]);
        rig.inject_event(Event::ConnectionRequest {
            addr: a,
            class_of_device: 0,
            link_type: 1,
        });
        rig.inject_event(Event::ConnectionComplete {
            status: 0,
            handle: 0x0100 + u16::from(n),
            addr: a,
            link_type: 1,
            encryption_enabled: false,
        });
    }
    assert!(rig.host.link_count() <= crate::link::MAX_LINKS);
    assert!(rig.traced("REFUSE reason=links"));
}

#[test]
fn a_sco_request_is_refused() {
    let (sim, _, _) = world();
    let mut rig = Rig::fresh(sim, Config::default());
    rig.inject_event(Event::ConnectionRequest {
        addr: addr(KBD),
        class_of_device: 0,
        link_type: 0,
    });
    assert!(rig.traced("REFUSE reason=not-acl"));
}

#[test]
fn data_held_for_an_encryption_that_never_comes_is_refused_after_the_wait() {
    // A bonded device that was offered its key and then asks for a HID
    // channel before the link is known to be encrypted is held for the
    // `Encryption_Change` (events and data take different USB endpoints).
    // When it never comes, the request is processed on an unencrypted link
    // and refused.
    let mut rig = bonded_rig();
    let kbd = addr(KBD);
    rig.inject_event(Event::ConnectionRequest {
        addr: kbd,
        class_of_device: 0x0540,
        link_type: 1,
    });
    rig.inject_event(Event::ConnectionComplete {
        status: 0,
        handle: 0x0050,
        addr: kbd,
        link_type: 1,
        encryption_enabled: false,
    });
    rig.inject_event(Event::LinkKeyRequest { addr: kbd });
    let sent = rig.inject_pdu(0x0050, &connect_request(0x11));
    assert!(sent.is_empty(), "held, not answered yet: {sent:?}");
    rig.capture = Some(Vec::new());
    rig.run_for(crate::link::DEFER_MS + 100);
    let sent = rig.capture.take().unwrap_or_default();
    let result = sent.iter().find_map(|p| connection_response(p));
    assert_eq!(
        result.map(|(result, _)| result),
        Some(3),
        "refused after the wait"
    );
    assert!(rig.traced("REFUSE reason=psm psm=0x0011"));
    assert!(rig.opened.is_empty());
}

#[test]
fn a_peer_that_talks_and_never_encrypts_is_dropped_not_buffered_forever() {
    let mut rig = bonded_rig();
    let kbd = addr(KBD);
    rig.inject_event(Event::ConnectionRequest {
        addr: kbd,
        class_of_device: 0x0540,
        link_type: 1,
    });
    rig.inject_event(Event::ConnectionComplete {
        status: 0,
        handle: 0x0050,
        addr: kbd,
        link_type: 1,
        encryption_enabled: false,
    });
    rig.inject_event(Event::LinkKeyRequest { addr: kbd });
    for _ in 0..crate::link::MAX_DEFERRED + 2 {
        rig.inject_pdu(0x0050, &connect_request(0x11));
    }
    assert!(rig.host.stats().overflow >= 1);
    assert!(rig
        .sim
        .commands_seen()
        .iter()
        .any(|c| matches!(c, Command::Disconnect { handle: 0x0050, .. })));
}

#[test]
fn an_unauthenticated_keyboard_bond_found_on_reconnect_is_forgotten() {
    // An interrupted pairing left a bond with no HID record, and the key was
    // not authenticated: the record says keyboard, so the bond goes.
    let rig = reconnect_with(|b| {
        b.authenticated = false;
        b.link_key.as_mut().unwrap().key_type = 4;
        b.report_descriptor.clear();
        b.hid_flags = 0;
    });
    assert!(
        rig.traced("REFUSE reason=keyboard-unauthenticated"),
        "{:#?}",
        rig.traces
    );
    assert!(
        rig.host.store().is_empty(),
        "the bond is forgotten, not retried at every boot"
    );
    assert!(rig.opened.is_empty() && rig.keys.is_empty());
    let saved = BondStore::decode(rig.saved.as_ref().unwrap()).unwrap();
    assert!(saved.is_empty(), "and so is the file");
}
