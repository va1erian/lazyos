//! HCI ACL data packets: `handle:12 | PB:2 | BC:2, length:u16, data`.

use alloc::vec::Vec;

use crate::wire::{put_u16, Reader};
use crate::Error;

/// Connection handles are 12 bits wide.
pub const HANDLE_MASK: u16 = 0x0FFF;

/// The packet-boundary flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    /// First fragment, not automatically flushable (host to controller).
    FirstNonFlushable,
    /// A continuation of the previous fragment.
    Continuing,
    /// First fragment, automatically flushable.
    FirstFlushable,
    /// Reserved (value 3): a complete PDU in a packet; not valid on this
    /// transport, kept so the caller can refuse it by name.
    Reserved,
}

impl Boundary {
    pub fn bits(self) -> u16 {
        match self {
            Boundary::FirstNonFlushable => 0,
            Boundary::Continuing => 1,
            Boundary::FirstFlushable => 2,
            Boundary::Reserved => 3,
        }
    }

    pub fn from_bits(v: u16) -> Boundary {
        match v & 3 {
            0 => Boundary::FirstNonFlushable,
            1 => Boundary::Continuing,
            2 => Boundary::FirstFlushable,
            _ => Boundary::Reserved,
        }
    }

    pub fn is_first(self) -> bool {
        matches!(self, Boundary::FirstNonFlushable | Boundary::FirstFlushable)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AclHeader {
    pub handle: u16,
    pub boundary: Boundary,
    /// Broadcast flag; point-to-point links use 0.
    pub broadcast: u8,
    pub len: u16,
}

/// Split an ACL packet into header and payload. The length field must match
/// the bytes present.
pub fn parse(packet: &[u8]) -> Result<(AclHeader, &[u8]), Error> {
    let mut r = Reader::new(packet);
    let word = r.u16()?;
    let len = r.u16()?;
    if r.remaining() != usize::from(len) {
        return Err(Error::BadLength);
    }
    let header = AclHeader {
        handle: word & HANDLE_MASK,
        boundary: Boundary::from_bits(word >> 12),
        broadcast: (word >> 14) as u8 & 3,
        len,
    };
    Ok((header, r.rest()))
}

/// Build an ACL packet. `data` longer than `u16::MAX` is refused.
pub fn encode(handle: u16, boundary: Boundary, data: &[u8]) -> Result<Vec<u8>, Error> {
    let len = u16::try_from(data.len()).map_err(|_| Error::TooLong)?;
    let mut out = Vec::with_capacity(4 + data.len());
    put_u16(&mut out, (handle & HANDLE_MASK) | boundary.bits() << 12);
    put_u16(&mut out, len);
    out.extend_from_slice(data);
    Ok(out)
}
