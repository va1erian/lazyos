//! A small SDP server: one HID service record, enough of the protocol for a
//! host to discover the report descriptor and the reconnect flags.
//!
//! This is the simulator's own encoding. The host-side `libs/btsdp` is a later
//! phase and must not depend on it, which is why the tests of that phase
//! check against bytes, not against this module's types.
//!
//! Requests are parsed defensively (the simulator is also the thing that
//! checks a host's requests are well-formed), and a continuation state is
//! the offset into the recomputed response, so responses larger than the
//! requested byte count (or the server's chunk size) really are split.

use std::vec::Vec;

use crate::hid::HidKind;

pub const PSM_SDP: u16 = 1;
pub const HID_RECORD_HANDLE: u32 = 0x0001_0000;

const BASE_UUID: u128 = 0x0000_0000_0000_1000_8000_0080_5F9B_34FB;
const UUID_L2CAP: u16 = 0x0100;
const UUID_HIDP: u16 = 0x0011;
const UUID_HID_SERVICE: u16 = 0x1124;
const UUID_PUBLIC_BROWSE_ROOT: u16 = 0x1002;

pub const fn uuid16(v: u16) -> u128 {
    BASE_UUID | (v as u128) << 96
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Elem {
    U8(u8),
    U16(u16),
    U32(u32),
    Uuid(u128),
    Text(Vec<u8>),
    Bool(bool),
    Seq(Vec<Elem>),
}

impl Elem {
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Elem::U8(v) => out.extend_from_slice(&[0x08, *v]),
            Elem::U16(v) => {
                out.push(0x09);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Elem::U32(v) => {
                out.push(0x0A);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Elem::Uuid(u) if *u & !(0xFFFF_u128 << 96) == BASE_UUID => {
                out.push(0x19);
                out.extend_from_slice(&((*u >> 96) as u16).to_be_bytes());
            }
            Elem::Uuid(u) => {
                out.push(0x1C);
                out.extend_from_slice(&u.to_be_bytes());
            }
            Elem::Text(t) => {
                out.push(0x26);
                out.extend_from_slice(&(t.len() as u16).to_be_bytes());
                out.extend_from_slice(t);
            }
            Elem::Bool(b) => out.extend_from_slice(&[0x28, u8::from(*b)]),
            Elem::Seq(items) => {
                let mut body = Vec::new();
                items.iter().for_each(|i| i.encode(&mut body));
                out.push(0x36);
                out.extend_from_slice(&(body.len() as u16).to_be_bytes());
                out.extend_from_slice(&body);
            }
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        self.encode(&mut v);
        v
    }
}

/// Parse one data element (types the requests use); returns it and its length.
fn parse_elem(b: &[u8], depth: u8) -> Option<(Elem, usize)> {
    let head = *b.first()?;
    let (ty, size) = (head >> 3, head & 7);
    let (hdr, len): (usize, usize) = match size {
        0..=4 if ty != 0 => (1, 1usize << size),
        5 => (2, usize::from(*b.get(1)?)),
        6 => (3, usize::from(u16::from_be_bytes([*b.get(1)?, *b.get(2)?]))),
        _ => return None,
    };
    let body = b.get(hdr..hdr.checked_add(len)?)?;
    let elem = match (ty, len) {
        (1, 1) => Elem::U8(body[0]),
        (1, 2) => Elem::U16(u16::from_be_bytes([body[0], body[1]])),
        (1, 4) => Elem::U32(u32::from_be_bytes([body[0], body[1], body[2], body[3]])),
        (3, 2) => Elem::Uuid(uuid16(u16::from_be_bytes([body[0], body[1]]))),
        (3, 4) => Elem::Uuid(
            BASE_UUID | u128::from(u32::from_be_bytes([body[0], body[1], body[2], body[3]])) << 96,
        ),
        (3, 16) => Elem::Uuid(u128::from_be_bytes(body.try_into().ok()?)),
        (6, _) if depth < 3 => {
            let mut items = Vec::new();
            let mut rest = body;
            while !rest.is_empty() && items.len() < 64 {
                let (e, n) = parse_elem(rest, depth + 1)?;
                items.push(e);
                rest = &rest[n..];
            }
            if !rest.is_empty() {
                return None;
            }
            Elem::Seq(items)
        }
        _ => Elem::Text(body.to_vec()),
    };
    Some((elem, hdr + len))
}

/// The attributes of the HID service record, in id order.
pub fn hid_record(
    kind: HidKind,
    reconnect_initiate: bool,
    normally_connectable: bool,
    name: &str,
) -> Vec<(u16, Elem)> {
    let l2cap = |psm| Elem::Seq(vec![Elem::Uuid(uuid16(UUID_L2CAP)), Elem::U16(psm)]);
    let hidp = Elem::Seq(vec![Elem::Uuid(uuid16(UUID_HIDP))]);
    let descriptor = Elem::Seq(vec![Elem::Seq(vec![
        Elem::U8(0x22),
        Elem::Text(kind.descriptor().to_vec()),
    ])]);
    vec![
        (0x0000, Elem::U32(HID_RECORD_HANDLE)),
        (
            0x0001,
            Elem::Seq(vec![Elem::Uuid(uuid16(UUID_HID_SERVICE))]),
        ),
        (0x0004, Elem::Seq(vec![l2cap(0x11), hidp.clone()])),
        (
            0x0005,
            Elem::Seq(vec![Elem::Uuid(uuid16(UUID_PUBLIC_BROWSE_ROOT))]),
        ),
        (
            0x0006,
            Elem::Seq(vec![
                Elem::U16(0x656E),
                Elem::U16(0x006A),
                Elem::U16(0x0100),
            ]),
        ),
        (
            0x0009,
            Elem::Seq(vec![Elem::Seq(vec![
                Elem::Uuid(uuid16(UUID_HID_SERVICE)),
                Elem::U16(0x0100),
            ])]),
        ),
        (0x000D, Elem::Seq(vec![Elem::Seq(vec![l2cap(0x13), hidp])])),
        (0x0100, Elem::Text(name.as_bytes().to_vec())),
        (0x0200, Elem::U16(0x0100)),
        (0x0201, Elem::U16(0x0111)),
        (0x0202, Elem::U8(kind.subclass())),
        (0x0203, Elem::U8(0)),
        (0x0204, Elem::Bool(true)),
        (0x0205, Elem::Bool(reconnect_initiate)),
        (0x0206, descriptor),
        (
            0x0207,
            Elem::Seq(vec![Elem::Seq(vec![Elem::U16(0x0409), Elem::U16(0x0100)])]),
        ),
        (0x020B, Elem::U16(0x0100)),
        (0x020C, Elem::U16(0x0C80)),
        (0x020D, Elem::Bool(normally_connectable)),
        (0x020E, Elem::Bool(true)),
    ]
}

mod error {
    pub const INVALID_RECORD_HANDLE: u16 = 0x0002;
    pub const INVALID_SYNTAX: u16 = 0x0003;
    pub const INVALID_CONTINUATION: u16 = 0x0005;
}

fn pdu(id: u8, tid: u16, params: &[u8]) -> Vec<u8> {
    let mut out = vec![id];
    out.extend_from_slice(&tid.to_be_bytes());
    out.extend_from_slice(&(params.len() as u16).to_be_bytes());
    out.extend_from_slice(params);
    out
}

fn error_pdu(tid: u16, code: u16) -> Vec<u8> {
    pdu(0x01, tid, &code.to_be_bytes())
}

/// Does a service search pattern name this record?
fn matches(pattern: &[Elem]) -> bool {
    pattern.iter().any(|e| {
        matches!(e, Elem::Uuid(u) if [UUID_HID_SERVICE, UUID_L2CAP, UUID_HIDP, UUID_PUBLIC_BROWSE_ROOT]
            .iter()
            .any(|w| *u == uuid16(*w)))
    })
}

/// The attributes the id list selects, as a data element sequence.
fn select(record: &[(u16, Elem)], ids: &[Elem]) -> Elem {
    let wanted = |id: u16| {
        ids.iter().any(|e| match e {
            Elem::U16(one) => *one == id,
            Elem::U32(range) => (*range >> 16) as u16 <= id && id <= *range as u16,
            _ => false,
        })
    };
    let mut flat = Vec::new();
    for (id, value) in record.iter().filter(|(id, _)| wanted(*id)) {
        flat.push(Elem::U16(*id));
        flat.push(value.clone());
    }
    Elem::Seq(flat)
}

struct Request<'a> {
    first: Elem,
    max: u16,
    second: Option<Elem>,
    continuation: Option<u16>,
    rest: &'a [u8],
}

/// Parse `elem, max:u16 [, elem], continuation`; `second` selects the layout.
fn parse_request(params: &[u8], with_second: bool) -> Option<Request<'_>> {
    let (first, n) = parse_elem(params, 0)?;
    let mut at = n;
    let max = u16::from_be_bytes([*params.get(at)?, *params.get(at + 1)?]);
    at += 2;
    let second = if with_second {
        let (e, n) = parse_elem(params.get(at..)?, 0)?;
        at += n;
        Some(e)
    } else {
        None
    };
    let cont_len = usize::from(*params.get(at)?);
    let cont = params.get(at + 1..at + 1 + cont_len)?;
    let continuation = match (cont_len, cont) {
        (0, _) => None,
        (2, [a, b]) => Some(u16::from_be_bytes([*a, *b])),
        _ => return None,
    };
    Some(Request {
        first,
        max,
        second,
        continuation,
        rest: &params[at + 1 + cont_len..],
    })
}

/// Answer one SDP request PDU. `chunk` caps the bytes per response so a test
/// can force continuation.
pub fn respond(request: &[u8], record: &[(u16, Elem)], chunk: usize) -> Vec<u8> {
    let (Some(&id), Some(tid), Some(plen)) = (
        request.first(),
        request.get(1..3).map(|b| u16::from_be_bytes([b[0], b[1]])),
        request
            .get(3..5)
            .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]]))),
    ) else {
        return error_pdu(0, error::INVALID_SYNTAX);
    };
    let Some(params) = request
        .get(5..5 + plen)
        .filter(|_| request.len() == 5 + plen)
    else {
        return error_pdu(tid, error::INVALID_SYNTAX);
    };
    match id {
        0x02 => service_search(tid, params),
        0x04 => service_attribute(tid, params, record, chunk),
        0x06 => service_search_attribute(tid, params, record, chunk),
        _ => error_pdu(tid, error::INVALID_SYNTAX),
    }
}

fn service_search(tid: u16, params: &[u8]) -> Vec<u8> {
    let Some(req) = parse_request(params, false) else {
        return error_pdu(tid, error::INVALID_SYNTAX);
    };
    let Elem::Seq(pattern) = &req.first else {
        return error_pdu(tid, error::INVALID_SYNTAX);
    };
    let hit = matches(pattern) && req.max > 0;
    let mut out = Vec::new();
    out.extend_from_slice(&u16::from(hit).to_be_bytes());
    out.extend_from_slice(&u16::from(hit).to_be_bytes());
    if hit {
        out.extend_from_slice(&HID_RECORD_HANDLE.to_be_bytes());
    }
    out.push(0);
    pdu(0x03, tid, &out)
}

fn chunked(tid: u16, id: u8, body: &[u8], req: &Request<'_>, chunk: usize) -> Vec<u8> {
    let start = usize::from(req.continuation.unwrap_or(0));
    if start > body.len() || !req.rest.is_empty() {
        return error_pdu(tid, error::INVALID_CONTINUATION);
    }
    let size = usize::from(req.max).min(chunk).min(body.len() - start);
    let end = start + size;
    let mut out = (size as u16).to_be_bytes().to_vec();
    out.extend_from_slice(&body[start..end]);
    if end < body.len() {
        out.push(2);
        out.extend_from_slice(&(end as u16).to_be_bytes());
    } else {
        out.push(0);
    }
    pdu(id, tid, &out)
}

fn service_attribute(tid: u16, params: &[u8], record: &[(u16, Elem)], chunk: usize) -> Vec<u8> {
    // ServiceRecordHandle:u32, MaxAttributeByteCount:u16, id list, continuation.
    let parsed = (|| {
        let handle = u32::from_be_bytes(params.get(..4)?.try_into().ok()?);
        let max = u16::from_be_bytes(params.get(4..6)?.try_into().ok()?);
        let (Elem::Seq(ids), n) = parse_elem(params.get(6..)?, 0)? else {
            return None;
        };
        Some((handle, ids, parse_tail(params.get(6 + n..)?, max)?))
    })();
    let Some((handle, ids, req)) = parsed else {
        return error_pdu(tid, error::INVALID_SYNTAX);
    };
    if handle != HID_RECORD_HANDLE {
        return error_pdu(tid, error::INVALID_RECORD_HANDLE);
    }
    chunked(tid, 0x05, &select(record, &ids).to_bytes(), &req, chunk)
}

/// The continuation field alone, with `max` already read.
fn parse_tail(tail: &[u8], max: u16) -> Option<Request<'_>> {
    let cont_len = usize::from(*tail.first()?);
    let cont = tail.get(1..1 + cont_len)?;
    let continuation = match (cont_len, cont) {
        (0, _) => None,
        (2, [a, b]) => Some(u16::from_be_bytes([*a, *b])),
        _ => return None,
    };
    Some(Request {
        first: Elem::Seq(Vec::new()),
        max,
        second: None,
        continuation,
        rest: &tail[1 + cont_len..],
    })
}

fn service_search_attribute(
    tid: u16,
    params: &[u8],
    record: &[(u16, Elem)],
    chunk: usize,
) -> Vec<u8> {
    let Some(req) = parse_request(params, true) else {
        return error_pdu(tid, error::INVALID_SYNTAX);
    };
    let (Elem::Seq(pattern), Some(Elem::Seq(ids))) = (&req.first, &req.second) else {
        return error_pdu(tid, error::INVALID_SYNTAX);
    };
    let lists = if matches(pattern) {
        vec![select(record, ids)]
    } else {
        Vec::new()
    };
    chunked(tid, 0x07, &Elem::Seq(lists).to_bytes(), &req, chunk)
}
