//! A simulated LE HID peripheral (HOGP): it advertises, accepts a connection
//! from the host, pairs with LE Secure Connections (Passkey Entry for the
//! keyboard, Just Works for the mouse), serves a GATT table and notifies
//! input reports once the host subscribed.
//!
//! Like [`crate::BrPeer`] it knows nothing of HCI: [`crate::SimController`]
//! carries ACL data to [`LePeer::rx_acl`] and runs the link layer around it
//! (advertising events, connection, the encryption procedure).

use std::collections::VecDeque;

use bthci::{BdAddr, Boundary};
use btl2cap::{Action, Config, Event, Handle, L2cap, LinkKind};
use btsmp::pdu::{AddrType, IoCapability, Pdu, Reason};
use btsmp::{func, DeviceAddr};

use crate::gatt::{GattServer, INPUT_REPORT};
use crate::hid::{self, HidKind};
use crate::peer::PasskeyBehavior;
use crate::smp_peer::SmpPeer;

const CID_ATT: u16 = 4;
const CID_SMP: u16 = 6;
/// Reports queued while the host is not subscribed.
const MAX_PENDING_REPORTS: usize = 16;

pub(crate) struct LeConn {
    pub handle: Handle,
    l2: L2cap,
    gatt: GattServer,
    smp: Option<SmpPeer>,
    pub encrypted: bool,
    /// The key the encryption procedure will be checked against when a
    /// pairing is in flight: `(ltk, authenticated)`.
    new_bond: Option<([u8; 16], u8)>,
    /// The identity keys have been sent after this link's encryption.
    keys_sent: bool,
}

pub struct LePeer {
    /// The identity address (public).
    pub addr: BdAddr,
    pub name: String,
    pub kind: HidKind,
    /// A keyboard has KeyboardOnly (Passkey Entry); `NoInputNoOutput` pairs
    /// with Just Works. A keyboard that says `NoInputNoOutput` is the device
    /// the host must refuse (plan Security section).
    pub io: IoCapability,
    pub passkey: PasskeyBehavior,
    /// Accepting a new bond (the user held the pairing button).
    pub pairing_mode: bool,
    /// Once bonded, advertise with a fresh resolvable private address each
    /// time (the default of every phone-class device, rare in keyboards).
    pub use_rpa: bool,
    /// With `use_rpa`, advertise from a resolvable private address even
    /// before any bond (a phone-class device that pairs from an RPA and
    /// then distributes its identity address).
    pub rpa_unbonded: bool,
    /// Distribute this identity address instead of the device's own (an
    /// impostor claiming another device's identity).
    pub claimed_identity: Option<BdAddr>,
    /// Misbehave: send an input notification the moment the link is up,
    /// before any encryption.
    pub early_notify: bool,
    /// Advertise the HID UUID and appearance (false: a decoy that must be
    /// ignored by a host that scans for HID devices).
    pub hid_adv: bool,
    /// How long after the link is up a misbehaving device sends its early
    /// notification (0: before the host's first command can be answered).
    pub greet_delay_ms: u64,
    pub irk: [u8; 16],
    /// The bond: the LTK and whether it is authenticated (1) or not (0).
    pub bond: Option<([u8; 16], u8)>,
    /// Server MTU; small values force long reads.
    pub gatt_mtu: u16,
    pub(crate) conn: Option<LeConn>,
    pub(crate) adv_until: Option<u64>,
    /// The address used by the current advertising session.
    pub(crate) adv: (u8, BdAddr),
    rpa_counter: u32,
    /// Reports not yet sent: `(report protocol bytes, boot protocol bytes)`.
    pending: VecDeque<(Vec<u8>, Vec<u8>)>,
    /// The host's SMP traffic was refused (a bonded device not in pairing
    /// mode, a malformed PDU).
    pub pairing_refused: u32,
    /// Attribute requests refused because the link was not encrypted.
    pub unencrypted_requests: u32,
}

impl LePeer {
    fn new(addr: [u8; 6], kind: HidKind, name: &str, io: IoCapability) -> LePeer {
        let addr = BdAddr::from_msb(addr);
        LePeer {
            addr,
            name: name.into(),
            kind,
            io,
            passkey: PasskeyBehavior::Correct,
            pairing_mode: false,
            use_rpa: false,
            rpa_unbonded: false,
            claimed_identity: None,
            early_notify: false,
            hid_adv: true,
            greet_delay_ms: 3,
            irk: addr.0.repeat(3)[..16].try_into().unwrap_or([0x77; 16]),
            bond: None,
            gatt_mtu: 64,
            conn: None,
            adv_until: None,
            adv: (0, addr),
            rpa_counter: 0,
            pending: VecDeque::new(),
            pairing_refused: 0,
            unencrypted_requests: 0,
        }
    }

    /// A keyboard: KeyboardOnly IO, so pairing uses Passkey Entry.
    pub fn keyboard(addr_msb: [u8; 6]) -> LePeer {
        LePeer::new(
            addr_msb,
            HidKind::Keyboard,
            "Sim LE Keyboard",
            IoCapability::KeyboardOnly,
        )
    }

    /// A mouse: NoInputNoOutput, so pairing is Just Works.
    pub fn mouse(addr_msb: [u8; 6]) -> LePeer {
        LePeer::new(
            addr_msb,
            HidKind::Mouse,
            "Sim LE Mouse",
            IoCapability::NoInputNoOutput,
        )
    }

    /// Enter pairing mode. Like most devices, this drops the old bond.
    pub fn set_pairing_mode(&mut self, on: bool) {
        self.pairing_mode = on;
        if on {
            self.bond = None;
        }
    }

    /// The address the current (or last) advertising session used.
    pub fn adv_address(&self) -> BdAddr {
        self.adv.1
    }

    pub fn is_connected(&self) -> bool {
        self.conn.is_some()
    }

    pub fn is_encrypted(&self) -> bool {
        self.conn.as_ref().is_some_and(|c| c.encrypted)
    }

    pub fn handle(&self) -> Option<Handle> {
        self.conn.as_ref().map(|c| c.handle)
    }

    /// Whether the host subscribed to the input reports: what makes the
    /// device "ready" the way both HID channels open make a BR/EDR one.
    pub fn subscribed(&self) -> bool {
        self.conn.as_ref().is_some_and(|c| {
            c.gatt.notifying(crate::gatt::INPUT_CCCD) || c.gatt.notifying(crate::gatt::BOOT_CCCD)
        })
    }

    /// The GATT server of the current link (for tests).
    pub fn gatt(&self) -> Option<&GattServer> {
        self.conn.as_ref().map(|c| &c.gatt)
    }

    /// Queue an input report. A notification carries no report id, and the
    /// bytes of the protocol in force are chosen when it is sent (the host
    /// may switch the device to boot protocol after the key was pressed).
    pub fn queue_report(&mut self, report: Vec<u8>, boot: Vec<u8>) {
        if self.pending.len() < MAX_PENDING_REPORTS {
            self.pending.push_back((report, boot));
        }
    }

    /// Start advertising until `until`; the address of the session is chosen
    /// now: a fresh RPA for a bonded device that uses them, else the
    /// identity.
    pub(crate) fn start_advertising(&mut self, until: u64) {
        let fresh = self.adv_until.is_none();
        self.adv_until = Some(until.max(self.adv_until.unwrap_or(0)));
        if !fresh {
            return;
        }
        self.adv = if self.use_rpa && (self.bond.is_some() || self.rpa_unbonded) {
            self.rpa_counter += 1;
            let n = self.rpa_counter.to_le_bytes();
            (
                1,
                BdAddr(func::make_rpa(&self.irk, [n[0] ^ 0x5A, n[1], n[2]])),
            )
        } else {
            (0, self.addr)
        };
    }

    /// The advertising data: flags, appearance, the HID service UUID, name.
    pub(crate) fn adv_data(&self) -> Vec<u8> {
        let mut d = vec![2, 0x01, 0x06];
        if self.hid_adv {
            let appearance: u16 = if self.kind == HidKind::Keyboard {
                0x03C1
            } else {
                0x03C2
            };
            d.extend([3, 0x19]);
            d.extend(appearance.to_le_bytes());
            d.extend([3, 0x03, 0x12, 0x18]);
        }
        let name = self.name.as_bytes();
        let n = name.len().min(31 - d.len() - 2);
        d.push(n as u8 + 1);
        d.push(0x09);
        d.extend_from_slice(&name[..n]);
        d
    }

    pub(crate) fn link_up(&mut self, handle: Handle, own: DeviceAddr, central: DeviceAddr) {
        let mut l2 = L2cap::new(Config::default());
        // A fresh L2CAP has room for one link.
        let _ = l2.link_up(handle, LinkKind::Le);
        self.adv_until = None;
        let identity = DeviceAddr {
            kind: AddrType::Public,
            addr: self.claimed_identity.unwrap_or(self.addr).0,
        };
        let smp = SmpPeer::new(self.io, own, central, identity, self.irk, &self.addr.0);
        self.conn = Some(LeConn {
            handle,
            l2,
            gatt: GattServer::hid(self.kind, &self.name, self.gatt_mtu),
            smp,
            encrypted: false,
            new_bond: None,
            keys_sent: false,
        });
    }

    pub(crate) fn link_down(&mut self) {
        self.conn = None;
    }

    /// What a misbehaving device sends the moment the link is up.
    pub(crate) fn greeting(&mut self) -> Vec<Vec<u8>> {
        if !self.early_notify {
            return Vec::new();
        }
        let report = hid::keyboard_report(0, 0x04, false);
        let pdu = crate::gatt::notification_pdu(INPUT_REPORT, without_id(&report));
        self.send_att(pdu)
    }

    fn send_on(&mut self, cid: u16, bytes: &[u8]) -> Vec<Vec<u8>> {
        let Some(c) = &mut self.conn else {
            return Vec::new();
        };
        let actions = c.l2.send(c.handle, cid, bytes).unwrap_or_default();
        actions
            .into_iter()
            .filter_map(|a| match a {
                Action::Send { pdu, .. } => Some(pdu),
                Action::Event(_) => None,
            })
            .collect()
    }

    fn send_att(&mut self, pdu: Vec<u8>) -> Vec<Vec<u8>> {
        self.send_on(CID_ATT, &pdu)
    }

    /// One ACL fragment from the host; returns L2CAP PDUs to send back.
    pub(crate) fn rx_acl(&mut self, now: u64, boundary: Boundary, data: &[u8]) -> Vec<Vec<u8>> {
        let Some(c) = &mut self.conn else {
            return Vec::new();
        };
        let actions = c.l2.rx_acl(now, c.handle, boundary, data);
        let mut out = Vec::new();
        for action in actions {
            match action {
                Action::Send { pdu, .. } => out.push(pdu),
                Action::Event(Event::Data { cid, data, .. }) => {
                    out.extend(self.on_data(cid, &data))
                }
                Action::Event(_) => {}
            }
        }
        out.extend(self.flush());
        out
    }

    fn on_data(&mut self, cid: u16, data: &[u8]) -> Vec<Vec<u8>> {
        match cid {
            CID_ATT => self.on_att(data),
            CID_SMP => self.on_smp(data),
            _ => Vec::new(),
        }
    }

    fn on_att(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        let Some(c) = &mut self.conn else {
            return Vec::new();
        };
        let before = c.gatt.refused_unencrypted;
        let reply = c.gatt.handle(c.encrypted, data);
        self.unencrypted_requests += c.gatt.refused_unencrypted - before;
        reply.map_or_else(Vec::new, |pdu| self.send_att(pdu))
    }

    fn on_smp(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        let is_request = matches!(Pdu::decode(data), Ok(Pdu::PairingRequest(_)));
        if is_request && !self.pairing_mode {
            // A bonded or unwilling device does not re-pair on request.
            self.pairing_refused += 1;
            return self.send_on(
                CID_SMP,
                &Pdu::PairingFailed(Reason::PAIRING_NOT_SUPPORTED).encode(),
            );
        }
        let Some(smp) = self.conn.as_mut().and_then(|c| c.smp.as_mut()) else {
            return Vec::new();
        };
        let result = smp.on_pdu(data);
        self.smp_replies(result)
    }

    fn smp_replies(&mut self, result: Result<Vec<Vec<u8>>, Reason>) -> Vec<Vec<u8>> {
        let replies = match result {
            Ok(replies) => replies,
            Err(reason) => vec![Pdu::PairingFailed(reason).encode()],
        };
        let mut out = Vec::new();
        for reply in replies {
            out.extend(self.send_on(CID_SMP, &reply));
        }
        // The key exists once the DHKey check passed.
        if let Some(c) = &mut self.conn {
            if let Some(smp) = &c.smp {
                if let (Some(ltk), None) = (smp.ltk, c.new_bond) {
                    c.new_bond = Some((ltk, u8::from(smp.authenticated())));
                }
            }
        }
        out
    }

    /// "The user typed the passkey the host shows."
    pub(crate) fn show_passkey(&mut self, shown: u32) -> Vec<Vec<u8>> {
        let typed = match self.passkey {
            PasskeyBehavior::Correct => shown,
            PasskeyBehavior::Wrong => (shown + 1) % 1_000_000,
            PasskeyBehavior::Never => return Vec::new(),
        };
        let Some(smp) = self.conn.as_mut().and_then(|c| c.smp.as_mut()) else {
            return Vec::new();
        };
        if !smp.wants_passkey() {
            return Vec::new();
        }
        let result = smp.type_passkey(typed);
        self.smp_replies(result)
    }

    /// The controller asks whether `ltk` is the key of this link: the stored
    /// bond's, or the one a pairing just made.
    pub(crate) fn accept_ltk(&self, ltk: &[u8; 16]) -> bool {
        let Some(c) = &self.conn else { return false };
        let candidates = [self.bond, c.new_bond];
        candidates.iter().flatten().any(|(key, _)| key == ltk)
    }

    /// The link is encrypted: commit a new bond, send the key distribution.
    pub(crate) fn on_encrypted(&mut self) -> Vec<Vec<u8>> {
        let Some(c) = &mut self.conn else {
            return Vec::new();
        };
        c.encrypted = true;
        if let Some(bond) = c.new_bond.take() {
            self.bond = Some(bond);
            self.pairing_mode = false;
        }
        let Some(c) = &mut self.conn else {
            return Vec::new();
        };
        let keys = match (&mut c.smp, c.keys_sent) {
            (Some(smp), false) => smp.on_encrypted(),
            _ => Vec::new(),
        };
        c.keys_sent = true;
        let mut out = Vec::new();
        for pdu in keys {
            out.extend(self.send_on(CID_SMP, &pdu));
        }
        out
    }

    /// Send queued reports if the host subscribed and the link is encrypted.
    pub(crate) fn flush(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(c) = &self.conn {
            if !c.encrypted || !self.subscribed() {
                break;
            }
            let Some((report, boot)) = self.pending.pop_front() else {
                break;
            };
            let bytes = if c.gatt.boot_protocol() { boot } else { report };
            let Some(pdu) = c.gatt.notification(&bytes) else {
                break;
            };
            out.extend(self.send_att(pdu));
        }
        out
    }
}

/// The key a mouse or keyboard sends without a report id: the report-protocol
/// bytes with the leading id removed.
pub fn without_id(report: &[u8]) -> &[u8] {
    report.get(1..).unwrap_or(&[])
}
