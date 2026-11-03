//! ATT PDUs (Core Specification, Vol 3 Part F): the requests a client sends
//! and the responses it reads.
//!
//! Everything a server sends is hostile. [`Response::parse`] is given the
//! request the response answers and checks the opcode, every length, that
//! entries fill the PDU exactly, and that every handle lies in the range that
//! was asked and ascends, so a discovery loop that follows the last handle
//! always makes progress.

use alloc::vec::Vec;

use crate::uuid::Uuid;
use crate::Error;

/// ATT opcodes.
pub mod op {
    pub const ERROR: u8 = 0x01;
    pub const MTU_REQ: u8 = 0x02;
    pub const MTU_RSP: u8 = 0x03;
    pub const FIND_INFO_REQ: u8 = 0x04;
    pub const FIND_INFO_RSP: u8 = 0x05;
    pub const READ_BY_TYPE_REQ: u8 = 0x08;
    pub const READ_BY_TYPE_RSP: u8 = 0x09;
    pub const READ_REQ: u8 = 0x0A;
    pub const READ_RSP: u8 = 0x0B;
    pub const READ_BLOB_REQ: u8 = 0x0C;
    pub const READ_BLOB_RSP: u8 = 0x0D;
    pub const READ_BY_GROUP_REQ: u8 = 0x10;
    pub const READ_BY_GROUP_RSP: u8 = 0x11;
    pub const WRITE_REQ: u8 = 0x12;
    pub const WRITE_RSP: u8 = 0x13;
    pub const NOTIFY: u8 = 0x1B;
    pub const INDICATE: u8 = 0x1D;
    pub const CONFIRM: u8 = 0x1E;
    pub const WRITE_CMD: u8 = 0x52;
    /// Bit 6: the PDU is a command (no response); bit 7: signed.
    pub const COMMAND_FLAG: u8 = 0x40;
    pub const SIGNED_FLAG: u8 = 0x80;
}

/// ATT error codes this crate names.
pub mod err {
    pub const INVALID_HANDLE: u8 = 0x01;
    pub const READ_NOT_PERMITTED: u8 = 0x02;
    pub const REQUEST_NOT_SUPPORTED: u8 = 0x06;
    pub const INVALID_OFFSET: u8 = 0x07;
    pub const ATTRIBUTE_NOT_FOUND: u8 = 0x0A;
    pub const ATTRIBUTE_NOT_LONG: u8 = 0x0B;
    pub const INSUFFICIENT_ENCRYPTION: u8 = 0x0F;
}

/// The default and the largest ATT MTU (the attribute value limit is 512).
pub const DEFAULT_MTU: u16 = 23;
pub const MAX_MTU: u16 = 517;

/// A request this client sends. One at a time (see `client.rs`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    ExchangeMtu { mtu: u16 },
    FindInformation { start: u16, end: u16 },
    ReadByType { start: u16, end: u16, uuid: Uuid },
    ReadByGroupType { start: u16, end: u16, uuid: Uuid },
    Read { handle: u16 },
    ReadBlob { handle: u16, offset: u16 },
    Write { handle: u16, value: Vec<u8> },
}

impl Request {
    pub fn opcode(&self) -> u8 {
        match self {
            Request::ExchangeMtu { .. } => op::MTU_REQ,
            Request::FindInformation { .. } => op::FIND_INFO_REQ,
            Request::ReadByType { .. } => op::READ_BY_TYPE_REQ,
            Request::ReadByGroupType { .. } => op::READ_BY_GROUP_REQ,
            Request::Read { .. } => op::READ_REQ,
            Request::ReadBlob { .. } => op::READ_BLOB_REQ,
            Request::Write { .. } => op::WRITE_REQ,
        }
    }

    fn response_opcode(&self) -> u8 {
        self.opcode() + 1
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut p = Vec::new();
        p.push(self.opcode());
        let u16s = |p: &mut Vec<u8>, vs: &[u16]| vs.iter().for_each(|v| p.extend(v.to_le_bytes()));
        match self {
            Request::ExchangeMtu { mtu } => u16s(&mut p, &[*mtu]),
            Request::FindInformation { start, end } => u16s(&mut p, &[*start, *end]),
            Request::ReadByType { start, end, uuid }
            | Request::ReadByGroupType { start, end, uuid } => {
                u16s(&mut p, &[*start, *end]);
                uuid.put(&mut p);
            }
            Request::Read { handle } => u16s(&mut p, &[*handle]),
            Request::ReadBlob { handle, offset } => u16s(&mut p, &[*handle, *offset]),
            Request::Write { handle, value } => {
                u16s(&mut p, &[*handle]);
                p.extend_from_slice(value);
            }
        }
        p
    }

    /// The handle range a response's handles must lie in.
    fn range(&self) -> (u16, u16) {
        match self {
            Request::FindInformation { start, end }
            | Request::ReadByType { start, end, .. }
            | Request::ReadByGroupType { start, end, .. } => (*start, *end),
            _ => (0, 0xFFFF),
        }
    }
}

/// An ATT Error Response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ErrorResponse {
    pub request_opcode: u8,
    pub handle: u16,
    pub code: u8,
}

/// One attribute group of a Read By Group Type response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub start: u16,
    pub end: u16,
    pub value: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    Mtu(u16),
    FindInformation(Vec<(u16, Uuid)>),
    ReadByType(Vec<(u16, Vec<u8>)>),
    ReadByGroupType(Vec<Group>),
    Read(Vec<u8>),
    ReadBlob(Vec<u8>),
    Write,
}

/// A bounds-checked cursor over a PDU.
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.0.len() < n {
            return Err(Error::Malformed);
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Error> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
}

/// Parse an Error Response PDU (opcode included).
pub fn parse_error(pdu: &[u8]) -> Result<ErrorResponse, Error> {
    let mut c = Cursor(pdu);
    if c.u8()? != op::ERROR {
        return Err(Error::Malformed);
    }
    let rsp = ErrorResponse {
        request_opcode: c.u8()?,
        handle: c.u16()?,
        code: c.u8()?,
    };
    if !c.0.is_empty() {
        return Err(Error::Malformed);
    }
    Ok(rsp)
}

impl Response {
    /// Parse `pdu` as the answer to `request`. `mtu` is the MTU in force,
    /// which bounds every value.
    pub fn parse(request: &Request, pdu: &[u8], mtu: u16) -> Result<Response, Error> {
        let mut c = Cursor(pdu);
        if c.u8()? != request.response_opcode() {
            return Err(Error::Unexpected);
        }
        if pdu.len() > usize::from(mtu.max(DEFAULT_MTU)) {
            return Err(Error::Malformed);
        }
        let (lo, hi) = request.range();
        match request {
            Request::ExchangeMtu { .. } => {
                let mtu = c.u16()?;
                done(&c, Response::Mtu(mtu))
            }
            Request::FindInformation { .. } => parse_find_info(c, lo, hi),
            Request::ReadByType { .. } => parse_by_type(c, lo, hi),
            Request::ReadByGroupType { .. } => parse_by_group(c, lo, hi),
            Request::Read { .. } => Ok(Response::Read(c.0.to_vec())),
            Request::ReadBlob { .. } => Ok(Response::ReadBlob(c.0.to_vec())),
            Request::Write { .. } => done(&c, Response::Write),
        }
    }
}

fn done(c: &Cursor<'_>, response: Response) -> Result<Response, Error> {
    if c.0.is_empty() {
        Ok(response)
    } else {
        Err(Error::Malformed)
    }
}

/// A handle in range and above the last one.
fn ascending(handle: u16, last: &mut u16, lo: u16, hi: u16) -> Result<(), Error> {
    if handle == 0 || handle < lo || handle > hi || handle <= *last {
        return Err(Error::Malformed);
    }
    *last = handle;
    Ok(())
}

fn parse_find_info(mut c: Cursor<'_>, lo: u16, hi: u16) -> Result<Response, Error> {
    let width = match c.u8()? {
        1 => 2,
        2 => 16,
        _ => return Err(Error::Malformed),
    };
    let mut out = Vec::new();
    let mut last = 0;
    while !c.0.is_empty() {
        let handle = c.u16()?;
        ascending(handle, &mut last, lo, hi)?;
        let uuid = Uuid::from_wire(c.take(width)?).ok_or(Error::Malformed)?;
        out.push((handle, uuid));
    }
    if out.is_empty() {
        return Err(Error::Malformed);
    }
    Ok(Response::FindInformation(out))
}

/// The entry length octet: it must hold a handle (or two) and divide the data.
fn entry_len(c: &mut Cursor<'_>, min: usize) -> Result<usize, Error> {
    let len = usize::from(c.u8()?);
    if len < min || c.0.is_empty() || c.0.len() % len != 0 {
        return Err(Error::Malformed);
    }
    Ok(len)
}

fn parse_by_type(mut c: Cursor<'_>, lo: u16, hi: u16) -> Result<Response, Error> {
    let len = entry_len(&mut c, 2)?;
    let mut out = Vec::new();
    let mut last = 0;
    while !c.0.is_empty() {
        let mut entry = Cursor(c.take(len)?);
        let handle = entry.u16()?;
        ascending(handle, &mut last, lo, hi)?;
        out.push((handle, entry.0.to_vec()));
    }
    Ok(Response::ReadByType(out))
}

fn parse_by_group(mut c: Cursor<'_>, lo: u16, hi: u16) -> Result<Response, Error> {
    let len = entry_len(&mut c, 4)?;
    let mut out = Vec::new();
    let mut last = 0;
    while !c.0.is_empty() {
        let mut entry = Cursor(c.take(len)?);
        let start = entry.u16()?;
        let end = entry.u16()?;
        ascending(start, &mut last, lo, hi)?;
        if end < start {
            return Err(Error::Malformed);
        }
        // The next group starts after this one ends.
        last = end;
        out.push(Group {
            start,
            end,
            value: entry.0.to_vec(),
        });
    }
    Ok(Response::ReadByGroupType(out))
}

/// What an unsolicited PDU from the server is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incoming {
    Notification { handle: u16, value: Vec<u8> },
    Indication { handle: u16, value: Vec<u8> },
    /// A request from the server (we are a client only): answer "not
    /// supported". Carries the request opcode.
    Request(u8),
    /// A command, a signed PDU or a confirmation: nothing to do.
    Ignored,
}

/// Parse a PDU that is not a response to our request.
pub fn parse_incoming(pdu: &[u8]) -> Result<Incoming, Error> {
    let mut c = Cursor(pdu);
    let opcode = c.u8()?;
    match opcode {
        op::NOTIFY | op::INDICATE => {
            let handle = c.u16()?;
            if handle == 0 {
                return Err(Error::Malformed);
            }
            let value = c.0.to_vec();
            Ok(if opcode == op::NOTIFY {
                Incoming::Notification { handle, value }
            } else {
                Incoming::Indication { handle, value }
            })
        }
        o if o & (op::COMMAND_FLAG | op::SIGNED_FLAG) != 0 => Ok(Incoming::Ignored),
        op::CONFIRM => Ok(Incoming::Ignored),
        // Even the responses we know are unsolicited here; a request opcode
        // from the server is anything odd below 0x20 that is not a response.
        o if o & 1 == 0 && o != 0 && o < 0x20 => Ok(Incoming::Request(o)),
        _ => Ok(Incoming::Ignored),
    }
}

/// An Error Response PDU.
pub fn error_pdu(request_opcode: u8, handle: u16, code: u8) -> Vec<u8> {
    let mut p = Vec::with_capacity(5);
    p.push(op::ERROR);
    p.push(request_opcode);
    p.extend_from_slice(&handle.to_le_bytes());
    p.push(code);
    p
}
