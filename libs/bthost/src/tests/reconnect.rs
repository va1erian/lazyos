//! Disconnect, reconnect and reboot: the bond makes the device type again
//! with no user action, and a link that goes releases every key.

use btbond::AddrKind;
use bthci::opcode::status;
use btsim::ReconnectStyle;

use super::*;
use crate::effect::SourceKind;

fn paired_keyboard() -> (Rig, PeerId) {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut rig = Rig::fresh(sim, pair_with(&[KBD]));
    rig.run_for(3_000);
    assert!(rig.host.is_typing(addr(KBD)), "{:?}", rig.traces);
    (rig, kbd)
}

#[test]
fn the_keyboard_reconnects_by_paging_us_and_the_first_key_arrives() {
    let (mut rig, kbd) = paired_keyboard();
    rig.sim.drop_link(kbd, rig.now, 0x08);
    rig.run_for(200);
    assert!(!rig.host.is_typing(addr(KBD)));
    assert!(rig.sources.is_empty(), "the source is closed with the link");
    assert!(rig.traced("CONN:DOWN"));
    rig.keys.clear();

    rig.sim.press_key(kbd, rig.now, KEY_A);
    rig.run_for(2_000);
    rig.assert_clean();
    assert!(rig.host.is_typing(addr(KBD)), "{:#?}", rig.traces);
    assert_eq!(
        rig.keys,
        [(4, true), (4, false)],
        "press and release survived"
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
fn after_a_reboot_the_bonded_keyboard_types_from_the_first_moment() {
    let (rig, kbd) = paired_keyboard();
    let mut sim_rig = rig.reboot(Config::default(), 10_000);
    // The user is already pressing a key while the host boots.
    sim_rig.sim.press_key(kbd, 10_000, KEY_A);
    sim_rig.run_for(2_000);
    sim_rig.assert_clean();
    assert!(sim_rig.host.is_typing(addr(KBD)));
    assert_eq!(sim_rig.typed(), [4], "{:?}", sim_rig.traces);
    let ready = sim_rig.trace_line("READY ms=").unwrap();
    let ms: u64 = ready["READY ms=".len()..].parse().unwrap();
    assert!(ms - sim_rig.started_at < 40, "ready at {ms}");
    assert!(sim_rig.traced("HID:KBD"));
    // The bond, not a pairing window, let it in.
    assert!(!sim_rig.traced("PAIR:START"));
    assert_eq!(sim_rig.sim.scan_enable(), 2);
}

#[test]
fn a_mouse_that_waits_to_be_paged_is_paged_after_boot() {
    let (mut sim, _, mouse) = world();
    sim.br_mut(mouse).set_pairing_mode(true);
    sim.br_mut(mouse).reconnect = ReconnectStyle::HostMustPage;
    let mut rig = Rig::fresh(sim, pair_with(&[MOUSE]));
    rig.run_for(3_000);
    assert!(rig.host.is_typing(addr(MOUSE)), "{:?}", rig.traces);
    let bond = rig.host.store().find(addr(MOUSE), AddrKind::BrEdr).unwrap();
    assert_eq!(bond.hid_flags & 1, 0, "the mouse does not page the host");
    assert_eq!(bond.hid_flags & 2, 2, "it is normally connectable");

    // Reboot: nobody touches the mouse, the host pages it.
    let mut rig = rig.reboot(Config::default(), 20_000);
    rig.run_for(3_000);
    rig.assert_clean();
    assert!(rig.host.is_typing(addr(MOUSE)), "{:?}", rig.traces);
    assert_eq!(rig.sources.len(), 1);
    assert_eq!(rig.sources[0].1, SourceKind::Pointer);
    assert!(!rig.traced("PAIR:START"));
}

#[test]
fn a_device_that_is_not_there_is_paged_a_bounded_number_of_times() {
    let (mut sim, _, mouse) = world();
    sim.br_mut(mouse).set_pairing_mode(true);
    sim.br_mut(mouse).reconnect = ReconnectStyle::HostMustPage;
    let mut rig = Rig::fresh(sim, pair_with(&[MOUSE]));
    rig.run_for(3_000);
    // The mouse is switched off: it forgets how to be connected to.
    rig.sim.br_mut(mouse).link_key = None;
    let pages = |rig: &Rig| {
        rig.sim
            .commands_seen()
            .iter()
            .filter(|c| matches!(c, bthci::Command::CreateConnection { .. }))
            .count()
    };
    let before = pages(&rig);
    let mut rig = rig.reboot(Config::default(), 20_000);
    rig.run_for(120_000);
    rig.assert_clean();
    assert!(!rig.host.is_typing(addr(MOUSE)));
    assert_eq!(
        pages(&rig) - before,
        usize::from(crate::paging::BOOT_ATTEMPTS),
        "bounded"
    );
    assert!(rig.traced("RECONNECT:GIVEUP addr=xx:xx:xx:xx:00:02 for=reconnect"));
    assert_eq!(rig.host.next_deadline(), None, "and then it sleeps");
}

#[test]
fn a_lost_link_to_a_device_that_waits_is_paged_again() {
    let (mut sim, _, mouse) = world();
    sim.br_mut(mouse).set_pairing_mode(true);
    sim.br_mut(mouse).reconnect = ReconnectStyle::HostMustPage;
    let mut rig = Rig::fresh(sim, pair_with(&[MOUSE]));
    rig.run_for(3_000);
    assert!(rig.host.is_typing(addr(MOUSE)));
    rig.sim.drop_link(mouse, rig.now, 0x08);
    rig.run_for(10_000);
    rig.assert_clean();
    assert!(rig.host.is_typing(addr(MOUSE)), "{:?}", rig.traces);
}

#[test]
fn link_loss_releases_every_held_key_before_the_source_closes() {
    let (mut rig, kbd) = paired_keyboard();
    // A key goes down and the keyboard walks out of range with it held.
    rig.sim
        .send_reports(kbd, vec![btsim::hid::keyboard_report(0x02, KEY_A, false)]);
    rig.run_for(50);
    assert_eq!(rig.keys, [(4, true), (0xE1, true)]);
    rig.sim.drop_link(kbd, rig.now, 0x08);
    rig.run_for(200);
    assert!(rig.sources.is_empty());
    assert_eq!(
        rig.kernel_released,
        Vec::<u16>::new(),
        "the host released the keys itself, the kernel had nothing left"
    );
    assert!(rig.keys.contains(&(4, false)) && rig.keys.contains(&(0xE1, false)));
}

#[test]
fn a_virtual_cable_unplug_forgets_the_device() {
    let (mut rig, kbd) = paired_keyboard();
    // The peer asked to unplug: the simulator sends HID_CONTROL on the
    // control channel when its flag is set while connected.
    rig.sim.unplug(kbd, rig.now);
    rig.run_for(500);
    assert!(rig.host.store().is_empty(), "{:?}", rig.traces);
    assert!(rig.traced("BOND:FORGET"));
    assert!(rig.sources.is_empty());
    assert_eq!(rig.host.link_count(), 0);
    let stored = btbond::BondStore::decode(rig.saved.as_ref().unwrap()).unwrap();
    assert!(stored.is_empty(), "the file forgets it too");
}

#[test]
fn an_idle_host_has_no_deadline_and_wakes_for_nothing() {
    let (rig, _) = paired_keyboard();
    assert_eq!(rig.host.next_deadline(), None);
    let _ = status::SUCCESS;
}

#[test]
fn a_peer_whose_first_request_beats_the_encryption_event_is_not_refused() {
    // Events and ACL data arrive on different USB endpoints, in no promised
    // order: the keyboard's connection request may reach the host before the
    // `Encryption_Change`. It is held for that event, not refused, and the
    // link is typing as soon as the host knows it is encrypted.
    let (rig, kbd) = paired_keyboard();
    let mut rig = rig.reboot(Config::default(), 10_000);
    rig.encryption_late = true;
    rig.sim.press_key(kbd, 10_000, KEY_A);
    rig.run_for(2_000);
    rig.assert_clean();
    assert!(rig.host.is_typing(addr(KBD)), "{:#?}", rig.traces);
    assert_eq!(rig.typed(), [4]);
    assert!(!rig.traced("REFUSE reason=psm"), "{:#?}", rig.traces);
    let line = rig.trace_line("RECONNECT ms=").expect("BT:RECONNECT");
    let ms: u64 = line["RECONNECT ms=".len()..]
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        ms < crate::link::OPEN_GRACE_MS,
        "not the fallback's doing: {ms} ms"
    );
}
