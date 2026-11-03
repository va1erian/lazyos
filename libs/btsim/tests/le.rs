//! The LE half of the model: what a scan hears, how the initiator connects, the
//! encryption procedure, and the rule of the HID service that nothing is
//! served on a link that is not encrypted.

use bthci::acl::{self, Boundary};
use bthci::event::AdvReport;
use bthci::opcode::status;
use bthci::{BdAddr, Command, Event};
use btsim::{Controller, HciIn, LePeer, PeerId, SimConfig, SimController};

const KBD: [u8; 6] = [0xC0, 0xFF, 0xEE, 0x00, 0x01, 0x01];
const LTK: [u8; 16] = [0x42; 16];

struct Raw {
    sim: SimController,
    now: u64,
    events: Vec<Event>,
    acl: Vec<Vec<u8>>,
}

impl Raw {
    fn new(sim: SimController) -> Raw {
        let mut raw = Raw {
            sim,
            now: 0,
            events: Vec::new(),
            acl: Vec::new(),
        };
        raw.cmd(Command::Reset);
        raw.cmd(Command::SetEventMask(bthci::opcode::EVENT_MASK_HID_HOST));
        raw.cmd(Command::LeSetEventMask(
            bthci::opcode::LE_EVENT_MASK_HID_HOST,
        ));
        raw
    }

    fn cmd(&mut self, c: Command) {
        self.sim.command(self.now, &c.encode());
        self.wait(20);
    }

    fn wait(&mut self, ms: u64) {
        self.now += ms;
        while let Some(p) = self.sim.poll_in(self.now) {
            match p {
                HciIn::Event(bytes) => self.events.push(Event::parse(&bytes).unwrap()),
                HciIn::Acl(bytes) => self.acl.push(bytes),
            }
        }
    }

    fn adverts(&self) -> Vec<&AdvReport> {
        self.events
            .iter()
            .filter_map(|e| match e {
                Event::LeAdvertisingReport(r) => Some(r),
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn connection(&self) -> Option<u16> {
        self.events.iter().find_map(|e| match e {
            Event::LeConnectionComplete(c) if c.status == 0 => Some(c.handle),
            _ => None,
        })
    }

    /// Send an ATT PDU on the fixed channel and return the answer's payload.
    fn att(&mut self, handle: u16, pdu: &[u8]) -> Option<Vec<u8>> {
        let mut l2 = (pdu.len() as u16).to_le_bytes().to_vec();
        l2.extend(4u16.to_le_bytes());
        l2.extend_from_slice(pdu);
        let packet = acl::encode(handle, Boundary::FirstNonFlushable, &l2).unwrap();
        self.acl.clear();
        self.sim.acl_out(self.now, &packet);
        self.wait(20);
        let answer = self.acl.first()?;
        let (_, data) = acl::parse(answer).unwrap();
        Some(data[4..].to_vec())
    }
}

fn bonded_keyboard() -> (SimController, PeerId) {
    let mut sim = SimController::new(SimConfig::default());
    let kbd = sim.add_le_peer(LePeer::keyboard(KBD));
    sim.set_bond(kbd, Some((LTK, 1)));
    (sim, kbd)
}

fn connect(raw: &mut Raw, kbd: PeerId) -> u16 {
    raw.cmd(Command::LeClearAcceptList);
    raw.cmd(Command::LeAddDeviceToAcceptList {
        address_type: 0,
        addr: BdAddr::from_msb(KBD),
    });
    raw.cmd(Command::LeCreateConnection {
        scan_interval: 0x60,
        scan_window: 0x30,
        filter_policy: 1,
        peer_address_type: 0,
        peer: BdAddr::default(),
        own_address_type: 0,
        interval_min: 0x18,
        interval_max: 0x28,
        latency: 0,
        supervision_timeout: 0x190,
        min_ce_length: 0,
        max_ce_length: 0,
    });
    raw.sim.wake(kbd, raw.now);
    raw.wait(200);
    raw.connection().expect("the initiator connected")
}

#[test]
fn a_touched_bonded_device_is_heard_by_a_scan_with_its_hid_advertisement() {
    let (sim, kbd) = bonded_keyboard();
    let mut raw = Raw::new(sim);
    raw.cmd(Command::LeSetScanParameters {
        active: false,
        interval: 0x60,
        window: 0x30,
        own_address_type: 0,
        filter_policy: 0,
    });
    raw.cmd(Command::LeSetScanEnable {
        enable: true,
        filter_duplicates: false,
    });
    assert!(raw.adverts().is_empty(), "nobody has touched it");
    raw.sim.wake(kbd, raw.now);
    raw.wait(200);
    let first = raw.adverts()[0].clone();
    assert_eq!((first.address_type, first.addr), (0, BdAddr::from_msb(KBD)));
    // Flags, appearance (keyboard), the HID service, the name.
    let data = &first.data;
    assert!(data.windows(4).any(|w| w == [3, 0x19, 0xC1, 0x03]));
    assert!(data.windows(4).any(|w| w == [3, 0x03, 0x12, 0x18]));
    assert!(data.len() <= 31);
    // A scan that is off hears nothing more.
    raw.cmd(Command::LeSetScanEnable {
        enable: false,
        filter_duplicates: false,
    });
    let heard = raw.adverts().len();
    raw.wait(300);
    assert_eq!(raw.adverts().len(), heard);
    assert!(raw.sim.violations().is_empty());
}

#[test]
fn an_unbonded_device_that_is_not_in_pairing_mode_stays_silent() {
    let mut sim = SimController::new(SimConfig::default());
    let kbd = sim.add_le_peer(LePeer::keyboard(KBD));
    let mut raw = Raw::new(sim);
    raw.cmd(Command::LeSetScanEnable {
        enable: true,
        filter_duplicates: false,
    });
    raw.sim.wake(kbd, raw.now);
    raw.wait(500);
    assert!(
        raw.adverts().is_empty(),
        "wake advertises only a bonded device"
    );
}

#[test]
fn the_initiator_connects_the_accept_list_and_cancel_ends_with_unknown_connection() {
    let (sim, kbd) = bonded_keyboard();
    let mut raw = Raw::new(sim);
    let handle = connect(&mut raw, kbd);
    assert!(raw.sim.le(kbd).is_connected());
    assert_eq!(raw.sim.link_handle(kbd), Some(handle));
    assert!(
        !raw.sim.le_connect_pending(),
        "a connection ends the initiator"
    );

    // A second initiator, cancelled: Command Complete, then the failed event.
    let (sim, _) = bonded_keyboard();
    let mut raw = Raw::new(sim);
    raw.cmd(Command::LeCreateConnection {
        scan_interval: 0x60,
        scan_window: 0x30,
        filter_policy: 1,
        peer_address_type: 0,
        peer: BdAddr::default(),
        own_address_type: 0,
        interval_min: 0x18,
        interval_max: 0x28,
        latency: 0,
        supervision_timeout: 0x190,
        min_ce_length: 0,
        max_ce_length: 0,
    });
    assert!(raw.sim.le_connect_pending());
    raw.cmd(Command::LeCreateConnectionCancel);
    assert!(!raw.sim.le_connect_pending());
    assert!(raw.events.iter().any(|e| matches!(
        e,
        Event::LeConnectionComplete(c) if c.status == status::UNKNOWN_CONNECTION
    )));
    // Nothing to cancel: Command Disallowed.
    raw.events.clear();
    raw.cmd(Command::LeCreateConnectionCancel);
    assert!(raw.events.iter().any(|e| matches!(
        e,
        Event::CommandComplete { return_params, .. } if return_params[0] == status::COMMAND_DISALLOWED
    )));
}

#[test]
fn the_wrong_key_fails_encryption_and_the_right_one_succeeds() {
    let (sim, kbd) = bonded_keyboard();
    let mut raw = Raw::new(sim);
    let handle = connect(&mut raw, kbd);
    raw.cmd(Command::LeStartEncryption {
        handle,
        random: 0,
        ediv: 0,
        ltk: [0x99; 16],
    });
    assert!(raw.events.iter().any(|e| matches!(
        e,
        Event::EncryptionChange { status: s, enabled: false, .. } if *s == status::PIN_OR_KEY_MISSING
    )));
    assert!(!raw.sim.link_encrypted(kbd));
    raw.cmd(Command::LeStartEncryption {
        handle,
        random: 0,
        ediv: 0,
        ltk: LTK,
    });
    assert!(raw.sim.link_encrypted(kbd));
    assert!(raw.events.iter().any(|e| matches!(
        e,
        Event::EncryptionChange {
            status: 0,
            enabled: true,
            ..
        }
    )));
}

#[test]
fn the_hid_service_is_not_served_on_a_link_that_is_not_encrypted() {
    let (sim, kbd) = bonded_keyboard();
    let mut raw = Raw::new(sim);
    let handle = connect(&mut raw, kbd);
    // Read the Report Map (handle 0x25): Insufficient Encryption.
    let answer = raw.att(handle, &[0x0A, 0x25, 0x00]).expect("an answer");
    assert_eq!(answer, [0x01, 0x0A, 0x25, 0x00, 0x0F]);
    // Exchange MTU is the one thing that is answered.
    let answer = raw.att(handle, &[0x02, 0xF7, 0x00]).expect("an answer");
    assert_eq!(answer[0], 0x03);
    raw.cmd(Command::LeStartEncryption {
        handle,
        random: 0,
        ediv: 0,
        ltk: LTK,
    });
    let answer = raw.att(handle, &[0x0A, 0x25, 0x00]).expect("an answer");
    assert_eq!(answer[0], 0x0B, "the Report Map after encryption");
    assert!(raw.sim.violations().is_empty());
}

#[test]
fn an_early_notifier_speaks_before_it_is_encrypted_and_a_polite_one_does_not() {
    for early in [true, false] {
        let (mut sim, kbd) = bonded_keyboard();
        sim.le_mut(kbd).early_notify = early;
        let mut raw = Raw::new(sim);
        connect(&mut raw, kbd);
        raw.wait(50);
        let spoke = raw.acl.iter().any(|p| {
            let (_, data) = acl::parse(p).unwrap();
            data.get(4) == Some(&0x1B)
        });
        assert_eq!(spoke, early);
    }
}

#[test]
fn the_ltk_request_event_is_not_used_when_the_host_is_the_central() {
    let (sim, kbd) = bonded_keyboard();
    let mut raw = Raw::new(sim);
    let handle = connect(&mut raw, kbd);
    raw.cmd(Command::LeStartEncryption {
        handle,
        random: 0,
        ediv: 0,
        ltk: LTK,
    });
    assert!(!raw
        .events
        .iter()
        .any(|e| matches!(e, Event::LeLongTermKeyRequest { .. })));
}
