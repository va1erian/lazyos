//! End to end: a scripted host brings the simulated controller up, pairs the
//! keyboard and the mouse, and reconnects them.

mod common;

use btbond::{AddrKind, BondStore};
use bthci::opcode::status;
use bthci::{BdAddr, Command, Phase};
use btsim::hid::{KEYBOARD_DESCRIPTOR, MOUSE_DESCRIPTOR};
use btsim::{BrPeer, PasskeyBehavior, PeerId, ReconnectStyle, SimConfig, SimController};
use common::{Host, Note};

const KBD: [u8; 6] = [0xC0, 0xFF, 0xEE, 0x00, 0x00, 0x01];
const MOUSE: [u8; 6] = [0xC0, 0xFF, 0xEE, 0x00, 0x00, 0x02];
const KEY_A: u8 = 0x04;

fn addr(msb: [u8; 6]) -> BdAddr {
    BdAddr::from_msb(msb)
}

fn world() -> (SimController, PeerId, PeerId) {
    let mut sim = SimController::new(SimConfig::default());
    let kbd = sim.add_br_peer(BrPeer::keyboard(KBD));
    let mouse = sim.add_br_peer(BrPeer::mouse(MOUSE));
    (sim, kbd, mouse)
}

fn boot_empty(sim: SimController) -> Host {
    let mut host = Host::boot(sim, BondStore::new(), 0);
    host.run_for(200);
    assert_eq!(host.init.phase(), Phase::Complete);
    host
}

/// Take the simulator back out of a host (a "power cycle" of the host only).
fn power_off(host: Host) -> (SimController, BondStore) {
    let bytes = host.store.encode().expect("bonds encode");
    let mut sim = host.sim;
    sim.power_cycle();
    (sim, BondStore::decode(&bytes).expect("bonds decode"))
}

#[test]
fn bring_up_puts_the_fast_path_first_and_breaks_no_rule() {
    let (sim, _, _) = world();
    let host = {
        let mut h = Host::boot(sim, BondStore::new(), 0);
        h.run_for(200);
        h
    };
    assert_eq!(host.init.phase(), Phase::Complete);
    assert!(
        host.sim.violations().is_empty(),
        "{:?}",
        host.sim.violations()
    );
    assert_eq!(host.sim.scan_enable(), 2, "page scan on");
    assert!(host.sim.ssp_enabled());
    assert_eq!(host.sim.event_mask(), bthci::opcode::EVENT_MASK_HID_HOST);
    assert_eq!(host.sim.local_name(), b"LazyOS");
    let seen = host.sim.commands_seen();
    let at =
        |pred: &dyn Fn(&Command) -> bool| seen.iter().position(pred).expect("command was sent");
    let scan = at(&|c| matches!(c, Command::WriteScanEnable(2)));
    let name = at(&|c| matches!(c, Command::WriteLocalName(_)));
    assert!(scan < name, "page scan before the cosmetic commands");
    let armed = host.armed_at.expect("armed");
    assert!(armed < 40, "armed {armed} ms after start");
    assert_eq!(host.init.info().bd_addr, Some(SimConfig::default().bd_addr));
}

#[test]
fn the_mouse_pairs_with_just_works_and_its_reports_arrive() {
    let (mut sim, _, mouse) = world();
    sim.br_mut(mouse).set_pairing_mode(true);
    let mut host = boot_empty(sim);
    host.connect(addr(MOUSE));
    host.run_for(1_000);

    assert!(
        host.sim.violations().is_empty(),
        "{:?}",
        host.sim.violations()
    );
    assert_eq!(
        host.jw_confirms, 1,
        "the user was asked to confirm, without a number to compare"
    );
    assert!(host.shown_passkey.is_none());
    let bond = host
        .store
        .find(addr(MOUSE), AddrKind::BrEdr)
        .expect("bonded");
    assert!(!bond.authenticated, "Just Works is an unauthenticated key");
    assert_eq!(bond.link_key.as_ref().unwrap().key_type, 4);
    assert!(host.sim.link_encrypted(mouse));
    assert_eq!(host.descriptor.as_deref(), Some(MOUSE_DESCRIPTOR));
    assert!(host.hid_ready(addr(MOUSE)));
    assert!(host.sim.br(mouse).hid_channels_open() == (true, true));
    assert!(
        host.sdp_requests >= 2,
        "the record needed a continuation: {}",
        host.sdp_requests
    );

    host.sim.move_mouse(mouse, host.now, 1, 5, -3);
    host.run_for(50);
    assert_eq!(host.reports, [vec![2, 1, 5, 0xFD, 0]]);
    assert!(host.sim.violations().is_empty());
}

#[test]
fn the_keyboard_pairs_with_passkey_entry() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut host = boot_empty(sim);
    host.connect(addr(KBD));
    host.run_for(2_000);

    assert!(
        host.sim.violations().is_empty(),
        "{:?}",
        host.sim.violations()
    );
    let passkey = host.shown_passkey.expect("the host displayed a passkey");
    assert!(passkey < 1_000_000);
    assert_eq!(host.jw_confirms, 0);
    let bond = host.store.find(addr(KBD), AddrKind::BrEdr).expect("bonded");
    assert!(bond.authenticated, "Passkey Entry is an authenticated key");
    assert_eq!(bond.link_key.as_ref().unwrap().key_type, 5);
    assert_eq!(host.descriptor.as_deref(), Some(KEYBOARD_DESCRIPTOR));
    assert!(host.hid_ready(addr(KBD)));

    host.sim.press_key(kbd, host.now, KEY_A);
    host.run_for(50);
    assert_eq!(
        host.reports,
        [
            vec![1, 0, 0, KEY_A, 0, 0, 0, 0, 0],
            vec![1, 0, 0, 0, 0, 0, 0, 0, 0]
        ]
    );
}

#[test]
fn a_wrongly_typed_passkey_fails_the_pairing() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    sim.br_mut(kbd).passkey = PasskeyBehavior::Wrong;
    let mut host = boot_empty(sim);
    host.connect(addr(KBD));
    host.run_for(2_000);
    assert!(host.has_note(&Note::PairingFailed(status::AUTHENTICATION_FAILURE)));
    assert!(host.has_note(&Note::AuthFailed(status::AUTHENTICATION_FAILURE)));
    assert!(host.store.is_empty());
    assert!(!host.hid_ready(addr(KBD)));
    assert!(!host.sim.link_encrypted(kbd));
}

#[test]
fn a_passkey_nobody_types_leaves_no_bond() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    sim.br_mut(kbd).passkey = PasskeyBehavior::Never;
    let mut host = boot_empty(sim);
    host.connect(addr(KBD));
    host.run_for(10_000);
    assert!(host.shown_passkey.is_some());
    assert!(host.store.is_empty());
    assert!(!host.hid_ready(addr(KBD)));
}

#[test]
fn an_unpairable_device_is_not_found() {
    let (sim, kbd, _) = world();
    let mut host = boot_empty(sim);
    host.connect(addr(KBD));
    host.run_for(3_000);
    assert!(host.has_note(&Note::ConnectFailed(status::PAGE_TIMEOUT)));
    assert!(host.sim.br(kbd).link_key.is_none());
}

fn pair_keyboard(sim: SimController) -> Host {
    let mut host = boot_empty(sim);
    host.connect(addr(KBD));
    host.run_for(2_000);
    assert!(host.hid_ready(addr(KBD)), "paired: {:?}", host.notes);
    host
}

#[test]
fn the_keyboard_reconnects_by_paging_the_host_and_its_first_key_arrives() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut host = pair_keyboard(sim);
    let handle = host.link_for(addr(KBD)).unwrap().handle;
    host.cmd(Command::Disconnect {
        handle,
        reason: status::REMOTE_USER_TERMINATED,
    });
    host.run_for(100);
    assert!(!host.hid_ready(addr(KBD)));
    assert!(host.has_note(&Note::Disconnected {
        handle,
        reason: status::CONNECTION_TERMINATED_BY_LOCAL_HOST
    }));
    host.reports.clear();

    // The user presses a key: the keyboard pages us, no Create_Connection.
    let creates = host
        .sim
        .commands_seen()
        .iter()
        .filter(|c| matches!(c, Command::CreateConnection { .. }))
        .count();
    host.sim.press_key(kbd, host.now, KEY_A);
    host.run_for(2_000);
    assert!(host.has_note(&Note::ConnectionRequest(addr(KBD))));
    assert_eq!(
        host.sim
            .commands_seen()
            .iter()
            .filter(|c| matches!(c, Command::CreateConnection { .. }))
            .count(),
        creates
    );
    assert!(host.hid_ready(addr(KBD)));
    assert_eq!(
        host.reports.len(),
        2,
        "press and release survived the reconnect: {:?}",
        host.reports
    );
    assert_eq!(host.reports[0][3], KEY_A);
    assert!(
        host.sim.violations().is_empty(),
        "{:?}",
        host.sim.violations()
    );
}

#[test]
fn a_device_we_hold_no_bond_for_is_refused_when_it_pages() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    let host = pair_keyboard(sim);
    // The host loses its bond store (a fresh install); the keyboard still has its key.
    let (sim, _) = power_off(host);
    let mut host = Host::boot(sim, BondStore::new(), 5_000);
    host.run_for(200);
    host.sim.press_key(kbd, host.now, KEY_A);
    host.run_for(1_000);
    assert!(
        host.has_note(&Note::ConnectionRequest(addr(KBD))),
        "{:?}",
        host.notes
    );
    assert!(!host.hid_ready(addr(KBD)));
    assert!(host.reports.is_empty());
}

#[test]
fn after_a_reboot_the_bonded_keyboard_types_from_the_first_moment() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    let host = pair_keyboard(sim);
    let (mut sim, store) = power_off(host);
    assert_eq!(store.len(), 1);

    // The host reboots; the user is already pressing a key while it boots.
    let t0 = 10_000;
    sim.press_key(kbd, t0, KEY_A);
    let mut host = Host::boot(sim, store, t0);
    host.run_for(2_000);

    assert!(host.hid_ready(addr(KBD)));
    assert_eq!(host.reports.len(), 2, "{:?}", host.reports);
    let armed = host.armed_at.expect("armed") - host.started_at;
    assert!(armed < 40, "armed within {armed} ms of boot");
    let admitted = host.first_request_ms.expect("the keyboard paged us") - host.started_at;
    assert!(
        admitted < 100,
        "the keyboard was admitted {admitted} ms after boot"
    );
    assert!(
        host.sim.violations().is_empty(),
        "{:?}",
        host.sim.violations()
    );
}

#[test]
fn a_mouse_that_waits_to_be_paged_is_paged_after_boot() {
    let (mut sim, _, mouse) = world();
    sim.br_mut(mouse).set_pairing_mode(true);
    sim.br_mut(mouse).reconnect = ReconnectStyle::HostMustPage;
    let mut host = boot_empty(sim);
    host.connect(addr(MOUSE));
    host.run_for(1_000);
    assert!(host.hid_ready(addr(MOUSE)));
    let handle = host.link_for(addr(MOUSE)).unwrap().handle;
    host.sim.drop_link(mouse, host.now, 0x08);
    host.run_for(100);
    assert!(host.has_note(&Note::Disconnected {
        handle,
        reason: 0x08
    }));
    host.reports.clear();

    // Pressing does nothing: the mouse waits. The host pages the bonded device.
    host.sim.move_mouse(mouse, host.now, 0, 1, 1);
    host.run_for(500);
    assert!(!host.hid_ready(addr(MOUSE)));
    host.connect(addr(MOUSE));
    host.run_for(1_000);
    assert!(host.hid_ready(addr(MOUSE)));
    assert!(
        host.sim.violations().is_empty(),
        "{:?}",
        host.sim.violations()
    );
}

#[test]
fn a_stale_link_key_is_refused_by_the_peer() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    sim.br_mut(kbd).reconnect = ReconnectStyle::HostMustPage;
    let mut host = pair_keyboard(sim);
    let handle = host.link_for(addr(KBD)).unwrap().handle;
    host.cmd(Command::Disconnect {
        handle,
        reason: 0x13,
    });
    host.run_for(100);
    // Corrupt the host's copy of the key.
    let mut bond = host.store.find(addr(KBD), AddrKind::BrEdr).unwrap().clone();
    bond.link_key.as_mut().unwrap().key[0] ^= 0xFF;
    host.store.upsert(bond).unwrap();
    host.connect(addr(KBD));
    host.run_for(1_000);
    assert!(
        host.has_note(&Note::AuthFailed(status::AUTHENTICATION_FAILURE)),
        "{:?}",
        host.notes
    );
    assert!(!host.hid_ready(addr(KBD)));
    assert!(!host.sim.link_encrypted(kbd));
}

#[test]
fn a_peer_that_lost_its_bond_is_not_trusted() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    sim.br_mut(kbd).reconnect = ReconnectStyle::HostMustPage;
    let mut host = pair_keyboard(sim);
    let handle = host.link_for(addr(KBD)).unwrap().handle;
    host.cmd(Command::Disconnect {
        handle,
        reason: 0x13,
    });
    host.run_for(100);
    host.sim.br_mut(kbd).link_key = None;
    host.sim.br_mut(kbd).reconnect = ReconnectStyle::HostMustPage;
    host.sim.br_mut(kbd).pairing_mode = true; // connectable, but no key to match
    host.connect(addr(KBD));
    host.run_for(1_000);
    assert!(
        host.has_note(&Note::AuthFailed(status::PIN_OR_KEY_MISSING)),
        "{:?}",
        host.notes
    );
    assert!(!host.hid_ready(addr(KBD)));
}

#[test]
fn a_device_that_has_not_paired_cannot_send_input() {
    let (sim, kbd, _) = world();
    let mut host = boot_empty(sim);
    host.sim.press_key(kbd, host.now, KEY_A);
    host.run_for(500);
    assert!(host.reports.is_empty());
    assert!(host.notes.is_empty());
}

#[test]
fn link_loss_is_reported_and_l2cap_cleans_up() {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut host = pair_keyboard(sim);
    let handle = host.link_for(addr(KBD)).unwrap().handle;
    host.sim.drop_link(kbd, host.now, 0x08);
    host.run_for(100);
    assert!(host.has_note(&Note::Disconnected {
        handle,
        reason: 0x08
    }));
    assert_eq!(host.l2.open_channels(handle), 0);
    assert!(host.link_for(addr(KBD)).is_none());
    assert_eq!(
        host.bufs.as_ref().unwrap().free(),
        host.init.info().acl.acl_num
    );
}
