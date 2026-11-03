//! A small GATT server for the tests: an attribute table that answers the
//! requests `btgatt` sends, with the knobs a hostile or quirky server needs.

use std::vec;
use std::vec::Vec;

use crate::att::{err, error_pdu, op};
use crate::uuid::{known, Uuid};

pub struct Attr {
    pub handle: u16,
    pub uuid: Uuid,
    pub value: Vec<u8>,
}

pub struct Server {
    pub attrs: Vec<Attr>,
    pub mtu: u16,
    /// Answer Exchange MTU with "not supported".
    pub no_mtu: bool,
    pub writes: Vec<(u16, Vec<u8>)>,
    pub commands: Vec<(u16, Vec<u8>)>,
}

fn s(v: u16) -> Uuid {
    Uuid::Short(v)
}

fn decl(props: u8, value: u16, uuid: u16) -> Vec<u8> {
    let mut v = vec![props];
    v.extend(value.to_le_bytes());
    v.extend(uuid.to_le_bytes());
    v
}

/// A keyboard-like device: GAP, Battery, then HID with an input report (id
/// 1), an output report, boot keyboard input, and a Report Map of `map`.
pub fn keyboard(map: &[u8]) -> Server {
    let mut a = Vec::new();
    let mut add = |handle, uuid, value: Vec<u8>| a.push(Attr { handle, uuid, value });
    add(0x01, s(known::PRIMARY_SERVICE), 0x1800u16.to_le_bytes().to_vec());
    add(0x02, s(known::CHARACTERISTIC), decl(0x02, 0x03, 0x2A00));
    add(0x03, s(0x2A00), b"Sim".to_vec());
    add(0x10, s(known::PRIMARY_SERVICE), 0x180Fu16.to_le_bytes().to_vec());
    add(0x11, s(known::CHARACTERISTIC), decl(0x12, 0x12, 0x2A19));
    add(0x12, s(0x2A19), vec![90]);
    add(0x13, s(known::CCCD), vec![0, 0]);
    add(0x20, s(known::PRIMARY_SERVICE), known::SERVICE_HID.to_le_bytes().to_vec());
    add(0x22, s(known::CHARACTERISTIC), decl(0x02, 0x23, known::HID_INFORMATION));
    add(0x23, s(known::HID_INFORMATION), vec![0x11, 0x01, 0, 0x02]);
    add(0x24, s(known::CHARACTERISTIC), decl(0x02, 0x25, known::REPORT_MAP));
    add(0x25, s(known::REPORT_MAP), map.to_vec());
    add(0x28, s(known::CHARACTERISTIC), decl(0x06, 0x29, known::PROTOCOL_MODE));
    add(0x29, s(known::PROTOCOL_MODE), vec![1]);
    add(0x2A, s(known::CHARACTERISTIC), decl(0x12, 0x2B, known::REPORT));
    add(0x2B, s(known::REPORT), vec![0; 8]);
    add(0x2C, s(known::CCCD), vec![0, 0]);
    add(0x2D, s(known::REPORT_REFERENCE), vec![1, 1]);
    add(0x2E, s(known::CHARACTERISTIC), decl(0x0E, 0x2F, known::REPORT));
    add(0x2F, s(known::REPORT), vec![0]);
    add(0x30, s(known::REPORT_REFERENCE), vec![1, 2]);
    add(0x31, s(known::CHARACTERISTIC), decl(0x12, 0x32, known::BOOT_KEYBOARD_INPUT));
    add(0x32, s(known::BOOT_KEYBOARD_INPUT), vec![0; 8]);
    add(0x33, s(known::CCCD), vec![0, 0]);
    Server {
        attrs: a,
        mtu: 64,
        no_mtu: false,
        writes: Vec::new(),
        commands: Vec::new(),
    }
}

impl Server {
    fn group_end(&self, start: u16) -> u16 {
        let next = self
            .attrs
            .iter()
            .find(|a| a.handle > start && a.uuid == s(known::PRIMARY_SERVICE));
        next.map_or_else(
            || self.attrs.last().map_or(start, |a| a.handle),
            |a| self.prev_handle(a.handle),
        )
    }

    fn prev_handle(&self, handle: u16) -> u16 {
        self.attrs
            .iter()
            .filter(|a| a.handle < handle)
            .map(|a| a.handle)
            .max()
            .unwrap_or(handle)
    }

    fn find(&self, handle: u16) -> Option<&Attr> {
        self.attrs.iter().find(|a| a.handle == handle)
    }

    fn u16_at(p: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([p[at], p[at + 1]])
    }

    /// Answer one request PDU (`None` for a command).
    pub fn handle(&mut self, p: &[u8]) -> Option<Vec<u8>> {
        let opcode = p[0];
        let max = usize::from(self.mtu) - 1;
        let nf = |handle| Some(error_pdu(opcode, handle, err::ATTRIBUTE_NOT_FOUND));
        match opcode {
            op::MTU_REQ if self.no_mtu => Some(error_pdu(opcode, 0, err::REQUEST_NOT_SUPPORTED)),
            op::MTU_REQ => {
                let mut r = vec![op::MTU_RSP];
                r.extend(self.mtu.to_le_bytes());
                Some(r)
            }
            op::READ_BY_GROUP_REQ => {
                let (start, end) = (Self::u16_at(p, 1), Self::u16_at(p, 3));
                let uuid = Uuid::from_wire(&p[5..])?;
                let groups: Vec<&Attr> = self
                    .attrs
                    .iter()
                    .filter(|a| a.uuid == uuid && (start..=end).contains(&a.handle))
                    .collect();
                let Some(first) = groups.first() else {
                    return nf(start);
                };
                let len = 4 + first.value.len();
                let mut r = vec![op::READ_BY_GROUP_RSP, len as u8];
                for g in groups.iter().filter(|g| g.value.len() == first.value.len()) {
                    if r.len() + len > max + 1 {
                        break;
                    }
                    r.extend(g.handle.to_le_bytes());
                    r.extend(self.group_end(g.handle).to_le_bytes());
                    r.extend(&g.value);
                }
                Some(r)
            }
            op::READ_BY_TYPE_REQ => {
                let (start, end) = (Self::u16_at(p, 1), Self::u16_at(p, 3));
                let uuid = Uuid::from_wire(&p[5..])?;
                let hits: Vec<&Attr> = self
                    .attrs
                    .iter()
                    .filter(|a| a.uuid == uuid && (start..=end).contains(&a.handle))
                    .collect();
                let Some(first) = hits.first() else {
                    return nf(start);
                };
                let len = 2 + first.value.len();
                let mut r = vec![op::READ_BY_TYPE_RSP, len as u8];
                for h in hits.iter().filter(|h| h.value.len() == first.value.len()) {
                    if r.len() + len > max + 1 {
                        break;
                    }
                    r.extend(h.handle.to_le_bytes());
                    r.extend(&h.value);
                }
                Some(r)
            }
            op::FIND_INFO_REQ => {
                let (start, end) = (Self::u16_at(p, 1), Self::u16_at(p, 3));
                let hits: Vec<&Attr> = self
                    .attrs
                    .iter()
                    .filter(|a| (start..=end).contains(&a.handle))
                    .collect();
                if hits.is_empty() {
                    return nf(start);
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
                let handle = Self::u16_at(p, 1);
                let offset = if opcode == op::READ_BLOB_REQ {
                    usize::from(Self::u16_at(p, 3))
                } else {
                    0
                };
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
            op::WRITE_REQ => {
                let handle = Self::u16_at(p, 1);
                self.writes.push((handle, p[3..].to_vec()));
                Some(vec![op::WRITE_RSP])
            }
            op::WRITE_CMD => {
                self.commands.push((Self::u16_at(p, 1), p[3..].to_vec()));
                None
            }
            _ => Some(error_pdu(opcode, 0, err::REQUEST_NOT_SUPPORTED)),
        }
    }
}
