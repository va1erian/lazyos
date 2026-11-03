//! The model itself: the rules it enforces on a host, and the behaviour of
//! its peers that the end-to-end tests do not reach.

mod common;

use btbond::{AddrKind, Bond, BondStore, Gatt, InputClass, Ltk};
use bthci::acl::{self, Boundary};
use bthci::opcode::{self, status};
use bthci::{BdAddr, Command, Event};
use btsim::{BrPeer, Controller, HciIn, SimConfig, SimController, Violation};
use common::Host;

const KBD: [u8; 6] = [0xC0, 0xFF, 0xEE, 0x00, 0x00, 0x01];

fn addr(msb: [u8; 6]) -> BdAddr {
    BdAddr::from_msb(msb)
}

/// Hand-driven access to a controller, one command at a time.
struct Raw {
    sim: SimController,
    now: u64,
    events: Vec<Event>,
}

impl Raw {
    fn new(sim: SimController) -> Raw {
        Raw {
            sim,
            now: 0,
            events: Vec::new(),
        }
    }

    /// Send a command and let 20 ms pass.
    fn cmd(&mut self, c: Command) {
        self.sim.command(self.now, &c.encode());
        self.wait(20);
    }

    fn wait(&mut self, ms: u64) {
        self.now += ms;
        while let Some(p) = self.sim.poll_in(self.now) {
            if let HciIn::Event(bytes) = p {
                self.events.push(Event::parse(&bytes).unwrap());
            }
        }
    }

    fn saw(&self, f: impl Fn(&Event) -> bool) -> bool {
        self.events.iter().any(f)
    }
}

#[test]
fn a_command_without_a_credit_is_recorded() {
    let mut sim = SimController::new(SimConfig::default());
    sim.command(0, &Command::Reset.encode());
    sim.command(0, &Command::ReadBdAddr.encode());
    assert_eq!(
        sim.violations(),
        [Violation::CommandWithoutCredit {
            opcode: opcode::READ_BD_ADDR
        }]
    );
    // The credit comes back with the answer.
    while sim.poll_in(100).is_some() {}
    assert_eq!(sim.credits(), 1);
    sim.command(100, &Command::ReadBdAddr.encode());
    assert_eq!(sim.violations().len(), 1);
}

#[test]
fn malformed_packets_are_recorded_not_obeyed() {
    let mut sim = SimController::new(SimConfig::default());
    sim.command(0, &[0x03, 0x0C, 0x05]);
    assert_eq!(sim.violations(), [Violation::MalformedCommand]);
    sim.acl_out(1, &[1, 2, 3]);
    assert!(sim.violations().contains(&Violation::MalformedAcl));
}

#[test]
fn events_the_host_did_not_unmask_are_not_delivered() {
    let mut sim = SimController::new(SimConfig::default());
    let kbd = sim.add_br_peer(BrPeer::keyboard(KBD));
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut raw = Raw::new(sim);
    raw.cmd(Command::Reset);
    raw.cmd(Command::WriteSimplePairingMode(true));
    let connect = Command::CreateConnection {
        addr: addr(KBD),
        packet_type: 0xCC18,
        page_scan_repetition_mode: 2,
        clock_offset: 0,
        allow_role_switch: 1,
    };
    raw.cmd(connect);
    raw.wait(100);
    let handle = raw
        .events
        .iter()
        .find_map(|e| match e {
            Event::ConnectionComplete {
                status: 0, handle, ..
            } => Some(*handle),
            _ => None,
        })
        .expect("connected: the connection events are in the default mask");
    raw.cmd(Command::AuthenticationRequested { handle });
    raw.cmd(Command::LinkKeyRequestNegativeReply { addr: addr(KBD) });
    raw.wait(100);
    assert!(raw.saw(|e| matches!(e, Event::LinkKeyRequest { .. })));
    assert!(
        !raw.saw(|e| matches!(e, Event::IoCapabilityRequest { .. })),
        "SSP events are off by default"
    );

    // After the mask is set the same step shows the event.
    raw.events.clear();
    raw.cmd(Command::SetEventMask(opcode::EVENT_MASK_HID_HOST));
    raw.cmd(Command::AuthenticationRequested { handle });
    raw.cmd(Command::LinkKeyRequestNegativeReply { addr: addr(KBD) });
    raw.wait(100);
    assert!(raw.saw(|e| matches!(e, Event::IoCapabilityRequest { .. })));
}

#[test]
fn reset_restores_the_defaults_and_drops_links() {
    let mut sim = SimController::new(SimConfig::default());
    let kbd = sim.add_br_peer(BrPeer::keyboard(KBD));
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut raw = Raw::new(sim);
    raw.cmd(Command::Reset);
    raw.cmd(Command::SetEventMask(opcode::EVENT_MASK_HID_HOST));
    raw.cmd(Command::WriteScanEnable(3));
    raw.cmd(Command::LeAddDeviceToAcceptList {
        address_type: 0,
        addr: addr(KBD),
    });
    raw.cmd(Command::Reset);
    assert_eq!(raw.sim.event_mask(), 0x1FFF_FFFF_FFFF);
    assert_eq!(raw.sim.scan_enable(), 0);
    assert!(raw.sim.accept_list().is_empty());
    assert!(raw.sim.violations().is_empty());
}

#[test]
fn acl_rules_are_enforced() {
    let mut sim = SimController::new(SimConfig {
        acl_num: 2,
        ..SimConfig::default()
    });
    let kbd = sim.add_br_peer(BrPeer::keyboard(KBD));
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut raw = Raw::new(sim);
    raw.cmd(Command::Reset);
    raw.cmd(Command::CreateConnection {
        addr: addr(KBD),
        packet_type: 0xCC18,
        page_scan_repetition_mode: 2,
        clock_offset: 0,
        allow_role_switch: 1,
    });
    raw.wait(100);
    let handle = raw
        .events
        .iter()
        .find_map(|e| {
            if let Event::ConnectionComplete { handle, .. } = e {
                Some(*handle)
            } else {
                None
            }
        })
        .unwrap();
    let pdu = [1u8, 0, 1, 0, 0xAA];
    let packet = acl::encode(handle, Boundary::FirstNonFlushable, &pdu).unwrap();
    raw.sim.acl_out(raw.now, &packet);
    raw.sim.acl_out(raw.now, &packet);
    assert!(raw.sim.violations().is_empty());
    raw.sim.acl_out(raw.now, &packet);
    assert_eq!(raw.sim.violations(), [Violation::AclNoBuffer { handle }]);
    // Completed packets come back as Number_Of_Completed_Packets.
    raw.wait(10);
    let completed: u16 = raw
        .events
        .iter()
        .filter_map(|e| {
            if let Event::NumberOfCompletedPackets(v) = e {
                Some(v.iter().map(|(_, n)| *n).sum::<u16>())
            } else {
                None
            }
        })
        .sum();
    assert_eq!(completed, 2);
    let big = acl::encode(handle, Boundary::FirstNonFlushable, &[0; 65]).unwrap();
    raw.sim.acl_out(raw.now, &big);
    assert!(raw
        .sim
        .violations()
        .contains(&Violation::AclTooLong { handle, len: 65 }));
    let stray = acl::encode(0x0999, Boundary::FirstNonFlushable, &pdu).unwrap();
    raw.sim.acl_out(raw.now, &stray);
    assert!(raw
        .sim
        .violations()
        .contains(&Violation::AclUnknownHandle { handle: 0x0999 }));
}

#[test]
fn a_br_edr_only_controller_refuses_le_commands_and_init_copes() {
    let sim = SimController::new(SimConfig {
        le_supported: false,
        ..SimConfig::default()
    });
    let mut host = Host::boot(sim, BondStore::new(), 0);
    host.run_for(300);
    assert_eq!(host.init.phase(), bthci::Phase::Complete);
    assert!(!host.init.info().le_supported);
    let le: Vec<_> = host
        .sim
        .commands_seen()
        .iter()
        .filter(|c| c.opcode() >> 10 == opcode::OGF_LE)
        .collect();
    assert_eq!(le, [&Command::LeReadBufferSize]);
    assert!(host.sim.violations().is_empty());
}

#[test]
fn le_bonds_become_the_accept_list_and_an_auto_connect() {
    let bond = Bond {
        addr: BdAddr([0xA1, 2, 3, 4, 5, 0xC6]),
        kind: AddrKind::LeRandom,
        authenticated: false,
        secure_connections: true,
        key_size: 16,
        link_key: None,
        ltk: Some(Ltk {
            key: [7; 16],
            ediv: 0,
            rand: 0,
        }),
        irk: Some([9; 16]),
        class_of_device: 0,
        input: InputClass::Pointer,
        hid_flags: 0,
        name: "LE mouse".into(),
        report_descriptor: Vec::new(),
        gatt: Gatt::default(),
    };
    let mut store = BondStore::new();
    store.upsert(bond.clone()).unwrap();
    let mut host = Host::boot(SimController::new(SimConfig::default()), store, 0);
    host.run_for(300);
    assert_eq!(host.sim.accept_list(), [(0, bond.addr)]);
    assert!(host.sim.le_connect_pending());
    assert!(host.sim.violations().is_empty());
}

fn paired_keyboard() -> (Host, btsim::PeerId) {
    let mut sim = SimController::new(SimConfig::default());
    let kbd = sim.add_br_peer(BrPeer::keyboard(KBD));
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut host = Host::boot(sim, BondStore::new(), 0);
    host.run_for(200);
    host.connect(addr(KBD));
    host.run_for(2_000);
    assert!(host.hid_ready(addr(KBD)));
    (host, kbd)
}

#[test]
fn the_peer_refuses_hid_channels_on_a_link_that_is_not_encrypted() {
    let mut sim = SimController::new(SimConfig::default());
    let kbd = sim.add_br_peer(BrPeer::keyboard(KBD));
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut host = Host::boot(sim, BondStore::new(), 0);
    host.auto_encrypt = false;
    host.run_for(200);
    host.connect(addr(KBD));
    host.run_for(2_000);
    let handle = host
        .link_for(addr(KBD))
        .expect("linked and authenticated")
        .handle;
    assert!(!host.sim.link_encrypted(kbd));
    let (_, actions) = host
        .l2
        .connect(host.now, handle, common::PSM_CONTROL)
        .unwrap();
    host.apply(actions);
    host.run_for(100);
    assert_eq!(host.sim.br(kbd).refused, 1);
    assert!(!host.hid_ready(addr(KBD)));
    // The SDP channel, which carries no input, is still open to anyone.
    let (_, actions) = host.l2.connect(host.now, handle, common::PSM_SDP).unwrap();
    host.apply(actions);
    host.run_for(200);
    assert!(host.sim.br(kbd).sdp_requests > 0);
}

#[test]
fn boot_protocol_reports_have_no_report_id() {
    let (mut host, kbd) = paired_keyboard();
    let link = host.link_for(addr(KBD)).unwrap();
    let (handle, control) = (link.handle, link.control.unwrap());
    host.send_on(handle, control, &[0x70]);
    host.run_for(50);
    assert!(host.sim.br(kbd).boot_protocol);
    host.reports.clear();
    host.sim.press_key(kbd, host.now, 0x05);
    host.run_for(50);
    assert_eq!(host.reports[0], [0, 0, 0x05, 0, 0, 0, 0, 0]);
}

#[test]
fn output_reports_and_virtual_cable_unplug() {
    let (mut host, kbd) = paired_keyboard();
    let link = host.link_for(addr(KBD)).unwrap();
    let (handle, control, interrupt) =
        (link.handle, link.control.unwrap(), link.interrupt.unwrap());
    host.send_on(handle, interrupt, &[0xA2, 1, 0x02]);
    host.run_for(50);
    assert_eq!(host.sim.br(kbd).output_reports, [vec![1, 0x02]]);
    host.send_on(handle, control, &[0x15]);
    host.run_for(50);
    assert!(host.sim.br(kbd).unplugged);
    assert!(
        host.sim.br(kbd).link_key.is_none(),
        "unplugging forgets the bond on the device"
    );
}

#[test]
fn the_same_seed_gives_the_same_world_and_another_seed_another_key() {
    fn run(seed: u64) -> (Vec<Event>, Option<u32>, Option<[u8; 16]>) {
        let mut sim = SimController::new(SimConfig {
            seed,
            ..SimConfig::default()
        });
        let kbd = sim.add_br_peer(BrPeer::keyboard(KBD));
        sim.br_mut(kbd).set_pairing_mode(true);
        let mut host = Host::boot(sim, BondStore::new(), 0);
        host.run_for(200);
        host.connect(addr(KBD));
        host.run_for(2_000);
        let key = host.sim.br(kbd).link_key.map(|k| k.0);
        (host.events.clone(), host.shown_passkey, key)
    }
    let a = run(1);
    assert_eq!(a, run(1));
    let b = run(2);
    assert_ne!(a.2, b.2);
    assert!(a.2.is_some());
}

#[test]
fn many_pair_reconnect_cycles_keep_the_books_balanced() {
    let (mut host, kbd) = paired_keyboard();
    for round in 0..50u64 {
        let handle = host.link_for(addr(KBD)).unwrap().handle;
        host.cmd(Command::Disconnect {
            handle,
            reason: status::REMOTE_USER_TERMINATED,
        });
        host.run_for(100);
        assert!(host.link_for(addr(KBD)).is_none(), "round {round}");
        host.sim.press_key(kbd, host.now, 0x04);
        host.run_for(1_000);
        assert!(host.hid_ready(addr(KBD)), "round {round}: {:?}", host.notes);
    }
    assert_eq!(host.reports.len(), 100);
    assert_eq!(
        host.bufs.as_ref().unwrap().free(),
        host.init.info().acl.acl_num
    );
    assert!(
        host.sim.violations().is_empty(),
        "{:?}",
        host.sim.violations()
    );
    assert_eq!(
        host.l2.violations(host.link_for(addr(KBD)).unwrap().handle),
        0
    );
}
