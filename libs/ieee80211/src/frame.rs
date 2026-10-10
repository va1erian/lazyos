//! Management frame parsing (IEEE 802.11-2020 9.2.4 and 9.3.3).
//!
//! A frame starts at the MAC header, without FCS. The 24-octet management
//! header is followed by four octets of HT Control when the Order bit is set
//! (they are skipped), then the body. Bodies borrow the input; the element
//! list inside is read with [`Elements::parse`].

use crate::ie::Elements;
use crate::{Error, Mac};

/// Management header length without HT Control.
pub const HEADER_LEN: usize = 24;

pub const SUB_ASSOC_REQ: u8 = 0;
pub const SUB_ASSOC_RESP: u8 = 1;
pub const SUB_PROBE_REQ: u8 = 4;
pub const SUB_PROBE_RESP: u8 = 5;
pub const SUB_BEACON: u8 = 8;
pub const SUB_DISASSOC: u8 = 10;
pub const SUB_AUTH: u8 = 11;
pub const SUB_DEAUTH: u8 = 12;
pub const SUB_ACTION: u8 = 13;

/// Authentication algorithm: Open System (9.4.1.1).
pub const AUTH_OPEN: u16 = 0;
/// Status code: success (9.4.1.9).
pub const STATUS_SUCCESS: u16 = 0;

/// The management MAC header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub subtype: u8,
    /// Frame control flags octet (To DS, From DS, Retry, ... Order).
    pub flags: u8,
    pub duration: u16,
    /// Receiver, transmitter, BSSID.
    pub addr1: Mac,
    pub addr2: Mac,
    pub addr3: Mac,
    pub sequence: u16,
}

impl Header {
    pub fn retry(&self) -> bool {
        self.flags & 0x08 != 0
    }

    /// The 12-bit sequence number (the raw field's fragment bits dropped).
    pub fn seq_number(&self) -> u16 {
        self.sequence >> 4
    }
}

/// Fixed fields and elements of a beacon or probe response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BeaconBody<'a> {
    pub timestamp: u64,
    /// In time units of 1024 microseconds.
    pub interval: u16,
    pub capability: u16,
    pub ies: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthBody<'a> {
    pub algorithm: u16,
    pub transaction: u16,
    pub status: u16,
    pub ies: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssocReqBody<'a> {
    pub capability: u16,
    pub listen_interval: u16,
    pub ies: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssocRespBody<'a> {
    pub capability: u16,
    pub status: u16,
    /// The raw field; the AID is its low 14 bits ([`AssocRespBody::aid`]).
    pub aid_field: u16,
    pub ies: &'a [u8],
}

impl AssocRespBody<'_> {
    pub fn aid(&self) -> u16 {
        self.aid_field & 0x3FFF
    }
}

/// A parsed management body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body<'a> {
    Beacon(BeaconBody<'a>),
    ProbeRequest {
        ies: &'a [u8],
    },
    ProbeResponse(BeaconBody<'a>),
    Auth(AuthBody<'a>),
    AssocRequest(AssocReqBody<'a>),
    AssocResponse(AssocRespBody<'a>),
    Deauth {
        reason: u16,
    },
    Disassoc {
        reason: u16,
    },
    /// An action frame: recognised, category and the rest left unread. The
    /// station ignores them safely (nothing here allocates or fails).
    Action {
        category: u8,
        rest: &'a [u8],
    },
    /// Any other management subtype (reassociation, ATIM, ...): recognised as
    /// management, not interpreted.
    Other {
        subtype: u8,
    },
}

/// A parsed management frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mgmt<'a> {
    pub header: Header,
    pub body: Body<'a>,
}

fn le16(buf: &[u8], at: usize) -> Result<u16, Error> {
    match buf.get(at..at + 2) {
        Some(&[a, b]) => Ok(u16::from_le_bytes([a, b])),
        _ => Err(Error::Short),
    }
}

fn mac(buf: &[u8], at: usize) -> Result<Mac, Error> {
    buf.get(at..at + 6)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(Error::Short)
}

impl<'a> Mgmt<'a> {
    /// Parse one management frame.
    ///
    /// Refused: a version other than 0, a non-management type, the Protected
    /// bit (the body is encrypted), a fragment (fragment number or More
    /// Fragments set), and anything shorter than its fixed fields.
    pub fn parse(frame: &'a [u8]) -> Result<Mgmt<'a>, Error> {
        if frame.len() < HEADER_LEN {
            return Err(Error::Short);
        }
        let (fc, flags) = (frame[0], frame[1]);
        if fc & 0x03 != 0 {
            return Err(Error::BadVersion);
        }
        if (fc >> 2) & 0x03 != 0 {
            return Err(Error::NotManagement);
        }
        if flags & 0x40 != 0 {
            return Err(Error::Protected);
        }
        let sequence = le16(frame, 22)?;
        if flags & 0x04 != 0 || sequence & 0x000F != 0 {
            return Err(Error::Fragmented);
        }
        let header = Header {
            subtype: fc >> 4,
            flags,
            duration: le16(frame, 2)?,
            addr1: mac(frame, 4)?,
            addr2: mac(frame, 10)?,
            addr3: mac(frame, 16)?,
            sequence,
        };
        // The Order bit means four octets of HT Control precede the body.
        let start = if flags & 0x80 != 0 {
            HEADER_LEN + 4
        } else {
            HEADER_LEN
        };
        let body = frame.get(start..).ok_or(Error::Short)?;
        Ok(Mgmt {
            header,
            body: parse_body(header.subtype, body)?,
        })
    }
}

fn parse_body(subtype: u8, body: &[u8]) -> Result<Body<'_>, Error> {
    Ok(match subtype {
        SUB_BEACON | SUB_PROBE_RESP => {
            let fixed = BeaconBody {
                timestamp: u64::from_le_bytes(
                    body.get(..8)
                        .and_then(|b| b.try_into().ok())
                        .ok_or(Error::Short)?,
                ),
                interval: le16(body, 8)?,
                capability: le16(body, 10)?,
                ies: &body[12..],
            };
            if subtype == SUB_BEACON {
                Body::Beacon(fixed)
            } else {
                Body::ProbeResponse(fixed)
            }
        }
        SUB_PROBE_REQ => Body::ProbeRequest { ies: body },
        SUB_AUTH => Body::Auth(AuthBody {
            algorithm: le16(body, 0)?,
            transaction: le16(body, 2)?,
            status: le16(body, 4)?,
            ies: &body[6..],
        }),
        SUB_ASSOC_REQ => Body::AssocRequest(AssocReqBody {
            capability: le16(body, 0)?,
            listen_interval: le16(body, 2)?,
            ies: &body[4..],
        }),
        SUB_ASSOC_RESP => Body::AssocResponse(AssocRespBody {
            capability: le16(body, 0)?,
            status: le16(body, 2)?,
            aid_field: le16(body, 4)?,
            ies: &body[6..],
        }),
        SUB_DEAUTH => Body::Deauth {
            reason: le16(body, 0)?,
        },
        SUB_DISASSOC => Body::Disassoc {
            reason: le16(body, 0)?,
        },
        SUB_ACTION => match body.split_first() {
            Some((&category, rest)) => Body::Action { category, rest },
            None => return Err(Error::Short),
        },
        other => Body::Other { subtype: other },
    })
}

impl<'a> Body<'a> {
    /// The element list of a body that has one.
    pub fn ies(&self) -> Option<&'a [u8]> {
        match *self {
            Body::Beacon(b) | Body::ProbeResponse(b) => Some(b.ies),
            Body::ProbeRequest { ies } => Some(ies),
            Body::Auth(b) => Some(b.ies),
            Body::AssocRequest(b) => Some(b.ies),
            Body::AssocResponse(b) => Some(b.ies),
            _ => None,
        }
    }

    /// The parsed elements of a body that has them.
    pub fn elements(&self) -> Result<Option<Elements<'a>>, Error> {
        self.ies().map(Elements::parse).transpose()
    }
}
