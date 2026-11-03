//! The GATT server of a simulated LE HID peripheral (HOGP): a fixed
//! attribute table, the ATT requests a client sends it, and the security rule
//! of the HID service: nothing but Exchange MTU is answered on a link that is
//! not encrypted (Error Response "Insufficient Encryption"), which is what a
//! real keyboard does and what lets a test tell a host that asked too early.
//!
//! The table (the same handles for both devices; a keyboard also has an
//! output report):
//!
//! ```text
//! 0x01 GAP service      0x03 Device Name
//! 0x10 Battery service  0x12 Battery Level (+ CCCD 0x13)
//! 0x20 HID service      0x23 HID Information   0x25 Report Map
//!                       0x27 HID Control Point 0x29 Protocol Mode
//!                       0x2B Report (input, CCCD 0x2C, Report Reference 0x2D)
//!                       0x2F Report (output, keyboard only; reference 0x30)
//!                       0x32 Boot Keyboard/Mouse Input (CCCD 0x33)
//! ```

use btgatt::att::{err, error_pdu, op};
use btgatt::uuid::{known, Uuid};

use crate::hid::{HidKind, KEYBOARD_REPORT_ID, MOUSE_REPORT_ID};

pub const INPUT_REPORT: u16 = 0x2B;
pub const INPUT_CCCD: u16 = 0x2C;
pub const BOOT_INPUT: u16 = 0x32;
pub const BOOT_CCCD: u16 = 0x33;
pub const PROTOCOL_MODE: u16 = 0x29;
const MAP: u16 = 0x25;

pub struct Attr {
    pub handle: u16,
    pub uuid: Uuid,
    pub value: Vec<u8>,
    pub writable: bool,
}

pub struct GattServer {
    attrs: Vec<Attr>,
    /// The MTU this server offers in Exchange MTU. Small values make the
    /// Report Map a long read.
    pub mtu: u16,
    negotiated: u16,
    /// Requests refused for want of encryption.
    pub refused_unencrypted: u32,
    /// Writes the client made: `(handle, value)`.
    pub writes: Vec<(u16, Vec<u8>)>,
    /// The opcode of every request or command received, in order (capped).
    pub requests: Vec<u8>,
}

fn short(v: u16) -> Uuid {
    Uuid::Short(v)
}

fn decl(props: u8, value: u16, uuid: u16) -> Vec<u8> {
    let mut v = vec![props];
    v.extend(value.to_le_bytes());
    v.extend(uuid.to_le_bytes());
    v
}

impl GattServer {
    pub fn hid(kind: HidKind, name: &str, mtu: u16) -> GattServer {
        let mut a: Vec<Attr> = Vec::new();
        let mut add = |handle, uuid: u16, value: Vec<u8>, writable| {
            a.push(Attr {
                handle,
                uuid: short(uuid),
                value,
                writable,
            })
        };
        let (primary, chr, cccd) = (known::PRIMARY_SERVICE, known::CHARACTERISTIC, known::CCCD);
        add(0x01, primary, 0x1800u16.to_le_bytes().to_vec(), false);
        add(0x02, chr, decl(0x02, 0x03, 0x2A00), false);
        add(0x03, 0x2A00, name.as_bytes().to_vec(), false);
        add(0x10, primary, 0x180Fu16.to_le_bytes().to_vec(), false);
        add(0x11, chr, decl(0x12, 0x12, 0x2A19), false);
        add(0x12, 0x2A19, vec![90], false);
        add(0x13, cccd, vec![0, 0], true);
        add(0x20, primary, known::SERVICE_HID.to_le_bytes().to_vec(), false);
        add(0x22, chr, decl(0x02, 0x23, known::HID_INFORMATION), false);
        // bcdHID 1.11, country 0, flags: normally connectable.
        add(0x23, known::HID_INFORMATION, vec![0x11, 0x01, 0, 0x02], false);
        add(0x24, chr, decl(0x02, MAP, known::REPORT_MAP), false);
        add(MAP, known::REPORT_MAP, kind.descriptor().to_vec(), false);
        add(0x26, chr, decl(0x04, 0x27, 0x2A4C), false);
        add(0x27, 0x2A4C, vec![0], true);
        add(0x28, chr, decl(0x06, PROTOCOL_MODE, known::PROTOCOL_MODE), false);
        add(PROTOCOL_MODE, known::PROTOCOL_MODE, vec![1], true);
        let id = match kind {
            HidKind::Keyboard => KEYBOARD_REPORT_ID,
            HidKind::Mouse => MOUSE_REPORT_ID,
        };
        add(0x2A, chr, decl(0x12, INPUT_REPORT, known::REPORT), false);
        add(INPUT_REPORT, known::REPORT, vec![0; 8], false);
        add(INPUT_CCCD, cccd, vec![0, 0], true);
        add(0x2D, known::REPORT_REFERENCE, vec![id, 1], false);
        if kind == HidKind::Keyboard {
            add(0x2E, chr, decl(0x0E, 0x2F, known::REPORT), false);
            add(0x2F, known::REPORT, vec![0], true);
            add(0x30, known::REPORT_REFERENCE, vec![id, 2], false);
        }
        let boot = match kind {
            HidKind::Keyboard => known::BOOT_KEYBOARD_INPUT,
            HidKind::Mouse => known::BOOT_MOUSE_INPUT,
        };
        add(0x31, chr, decl(0x12, BOOT_INPUT, boot), false);
        add(BOOT_INPUT, boot, vec![0; 8], false);
        add(BOOT_CCCD, cccd, vec![0, 0], true);
        GattServer {
            attrs: a,
            mtu,
            negotiated: 23,
            refused_unencrypted: 0,
            writes: Vec::new(),
            requests: Vec::new(),
        }
    }

    pub fn attrs(&self) -> &[Attr] {
        &self.attrs
    }

    fn find(&self, handle: u16) -> Option<&Attr> {
        self.attrs.iter().find(|a| a.handle == handle)
    }

    /// Whether the client switched notifications on for the CCCD `cccd`.
    pub fn notifying(&self, cccd: u16) -> bool {
        self.find(cccd).is_some_and(|a| a.value.first().is_some_and(|v| v & 1 != 0))
    }

    /// The device was switched to the boot protocol with Protocol Mode.
    pub fn boot_protocol(&self) -> bool {
        self.find(PROTOCOL_MODE).is_some_and(|a| a.value == [0])
    }

    /// A Handle Value Notification for `report` (without a report id) on the
    /// input characteristic of the protocol in force, if the client enabled it.
    pub fn notification(&self, report: &[u8]) -> Option<Vec<u8>> {
        let (value, cccd) = if self.boot_protocol() {
            (BOOT_INPUT, BOOT_CCCD)
        } else {
            (INPUT_REPORT, INPUT_CCCD)
        };
        self.notifying(cccd).then(|| notification_pdu(value, report))
    }

    fn group_end(&self, start: u16) -> u16 {
        let next = self
            .attrs
            .iter()
            .find(|a| a.handle > start && a.uuid == short(known::PRIMARY_SERVICE));
        let last = self.attrs.last().map_or(start, |a| a.handle);
        next.map_or(last, |n| {
            self.attrs
                .iter()
                .filter(|a| a.handle < n.handle)
                .map(|a| a.handle)
                .max()
                .unwrap_or(start)
        })
    }

    fn u16_at(p: &[u8], at: usize) -> Option<u16> {
        Some(u16::from_le_bytes([*p.get(at)?, *p.get(at + 1)?]))
    }

    /// Answer one ATT PDU; `None` for a command.
    pub fn handle(&mut self, encrypted: bool, p: &[u8]) -> Option<Vec<u8>> {
        let opcode = *p.first()?;
        if self.requests.len() < 256 {
            self.requests.push(opcode);
        }
        if opcode == op::MTU_REQ {
            let client = Self::u16_at(p, 1)?;
            self.negotiated = client.min(self.mtu).max(23);
            let mut r = vec![op::MTU_RSP];
            r.extend(self.mtu.to_le_bytes());
            return Some(r);
        }
        let command = opcode & op::COMMAND_FLAG != 0;
        if !encrypted {
            self.refused_unencrypted += 1;
            return (!command).then(|| error_pdu(opcode, 0, err::INSUFFICIENT_ENCRYPTION));
        }
        self.respond(opcode, p)
    }

    fn respond(&mut self, opcode: u8, p: &[u8]) -> Option<Vec<u8>> {
        let max = usize::from(self.negotiated) - 1;
        let not_found = |handle| Some(error_pdu(opcode, handle, err::ATTRIBUTE_NOT_FOUND));
        match opcode {
            op::READ_BY_GROUP_REQ | op::READ_BY_TYPE_REQ => {
                let (start, end) = (Self::u16_at(p, 1)?, Self::u16_at(p, 3)?);
                let uuid = Uuid::from_wire(p.get(5..)?)?;
                let hits: Vec<&Attr> = self
                    .attrs
                    .iter()
                    .filter(|a| a.uuid == uuid && (start..=end).contains(&a.handle))
                    .collect();
                let Some(first) = hits.first() else {
                    return not_found(start);
                };
                let group = opcode == op::READ_BY_GROUP_REQ;
                let len = if group { 4 } else { 2 } + first.value.len();
                let mut r = vec![opcode + 1, len as u8];
                for h in hits.iter().filter(|h| h.value.len() == first.value.len()) {
                    if r.len() + len > max + 1 {
                        break;
                    }
                    r.extend(h.handle.to_le_bytes());
                    if group {
                        r.extend(self.group_end(h.handle).to_le_bytes());
                    }
                    r.extend(&h.value);
                }
                Some(r)
            }
            op::FIND_INFO_REQ => {
                let (start, end) = (Self::u16_at(p, 1)?, Self::u16_at(p, 3)?);
                let hits: Vec<&Attr> = self
                    .attrs
                    .iter()
                    .filter(|a| (start..=end).contains(&a.handle))
                    .collect();
                if hits.is_empty() {
                    return not_found(start);
                }
                let mut r = vec![op::FIND_INFO_RSP, 1];
                for h in hits {
                    if r.len() + 4 > max + 1 {
                        break;
                    }
                    r.extend(h.handle.to_le_bytes());
                    h.uuid.put(&mut r);
                }
                Some(r)
            }
            op::READ_REQ | op::READ_BLOB_REQ => {
                let handle = Self::u16_at(p, 1)?;
                let blob = opcode == op::READ_BLOB_REQ;
                let offset = if blob { usize::from(Self::u16_at(p, 3)?) } else { 0 };
                let Some(a) = self.find(handle) else {
                    return Some(error_pdu(opcode, handle, err::INVALID_HANDLE));
                };
                if offset > a.value.len() {
                    return Some(error_pdu(opcode, handle, err::INVALID_OFFSET));
                }
                let end = a.value.len().min(offset + max);
                let mut r = vec![opcode + 1];
                r.extend(&a.value[offset..end]);
                Some(r)
            }
            op::WRITE_REQ | op::WRITE_CMD => {
                let handle = Self::u16_at(p, 1)?;
                let value = p.get(3..)?.to_vec();
                let ok = self.find(handle).is_some_and(|a| a.writable);
                if ok {
                    self.writes.push((handle, value.clone()));
                    if let Some(a) = self.attrs.iter_mut().find(|a| a.handle == handle) {
                        a.value = value;
                    }
                }
                match (opcode, ok) {
                    (op::WRITE_CMD, _) => None,
                    (_, true) => Some(vec![op::WRITE_RSP]),
                    (_, false) => Some(error_pdu(opcode, handle, 0x03)),
                }
            }
            _ => Some(error_pdu(opcode, 0, err::REQUEST_NOT_SUPPORTED)),
        }
    }
}

/// A Handle Value Notification PDU.
pub fn notification_pdu(handle: u16, value: &[u8]) -> Vec<u8> {
    let mut p = vec![op::NOTIFY];
    p.extend(handle.to_le_bytes());
    p.extend_from_slice(value);
    p
}
