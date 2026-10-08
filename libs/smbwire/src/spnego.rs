//! The slice of SPNEGO (RFC 4178, with Microsoft's `NegTokenInit2` hint) an
//! NTLM-only client needs: read the server's mechanism list and its
//! `NegTokenResp`, and wrap the client's NTLM messages the same way.
//!
//! The DER reader is strict about lengths (definite form, at most four length
//! bytes, never past its parent) and ignores fields it does not need.

use alloc::vec::Vec;

use crate::Error;

/// `1.3.6.1.5.5.2`, SPNEGO.
const SPNEGO_OID: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x02];
/// `1.3.6.1.4.1.311.2.2.10`, NTLMSSP.
pub const NTLMSSP_OID: &[u8] = &[0x2b, 0x06, 0x01, 0x04, 0x01, 0x82, 0x37, 0x02, 0x02, 0x0a];

/// `negState` values.
pub const ACCEPT_COMPLETED: u8 = 0;
pub const ACCEPT_INCOMPLETE: u8 = 1;
pub const REJECT: u8 = 2;
pub const REQUEST_MIC: u8 = 3;

const BAD: Error = Error::Malformed("SPNEGO token");

/// One DER element: its tag and contents, and what follows it.
fn element(bytes: &[u8]) -> Result<(u8, &[u8], &[u8]), Error> {
    let (&tag, rest) = bytes.split_first().ok_or(BAD)?;
    let (&first, rest) = rest.split_first().ok_or(BAD)?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let count = (first & 0x7f) as usize;
        if count == 0 || count > 4 || rest.len() < count {
            return Err(BAD);
        }
        let len = rest[..count]
            .iter()
            .fold(0usize, |acc, &b| (acc << 8) | b as usize);
        (len, &rest[count..])
    };
    if rest.len() < len {
        return Err(BAD);
    }
    Ok((tag, &rest[..len], &rest[len..]))
}

/// The contents of the element tagged `want` among `bytes`' elements.
fn find(mut bytes: &[u8], want: u8) -> Result<Option<&[u8]>, Error> {
    while !bytes.is_empty() {
        let (tag, contents, rest) = element(bytes)?;
        if tag == want {
            return Ok(Some(contents));
        }
        bytes = rest;
    }
    Ok(None)
}

/// The contents of `bytes`, which must be one element tagged `want`.
fn only(bytes: &[u8], want: u8) -> Result<&[u8], Error> {
    let (tag, contents, _) = element(bytes)?;
    if tag != want {
        return Err(BAD);
    }
    Ok(contents)
}

/// What the server's NEGOTIATE response said about authentication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint {
    /// No security buffer: the client speaks raw NTLMSSP.
    None,
    /// A SPNEGO hint listing NTLMSSP: the client wraps its messages.
    Spnego,
}

/// Read a NEGOTIATE response's security buffer.
pub fn parse_hint(buffer: &[u8]) -> Result<Hint, Error> {
    if buffer.is_empty() {
        return Ok(Hint::None);
    }
    let inner = only(buffer, 0x60)?;
    let (tag, oid, rest) = element(inner)?;
    if tag != 0x06 || oid != SPNEGO_OID {
        return Err(Error::Refused("the server's security token is not SPNEGO"));
    }
    let init = only(only(rest, 0xa0)?, 0x30)?;
    let types = find(init, 0xa0)?.ok_or(BAD)?;
    let mut list = only(types, 0x30)?;
    while !list.is_empty() {
        let (tag, oid, rest) = element(list)?;
        if tag == 0x06 && oid == NTLMSSP_OID {
            return Ok(Hint::Spnego);
        }
        list = rest;
    }
    Err(Error::Refused(
        "the server does not offer NTLM authentication",
    ))
}

/// A server SESSION_SETUP token, unwrapped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply<'a> {
    /// Whether it came in SPNEGO (the client answers in kind).
    pub wrapped: bool,
    pub state: Option<u8>,
    /// The NTLM message inside, if any.
    pub token: Option<&'a [u8]>,
}

/// Read a SESSION_SETUP response's security buffer: a `NegTokenResp`, or a
/// raw NTLMSSP message.
pub fn parse_reply(buffer: &[u8]) -> Result<Reply<'_>, Error> {
    if buffer.starts_with(crate::ntlm::SIGNATURE) {
        return Ok(Reply {
            wrapped: false,
            state: None,
            token: Some(buffer),
        });
    }
    if buffer.is_empty() {
        return Ok(Reply {
            wrapped: false,
            state: None,
            token: None,
        });
    }
    let resp = only(only(buffer, 0xa1)?, 0x30)?;
    let state = match find(resp, 0xa0)? {
        Some(field) => {
            let value = only(field, 0x0a)?;
            match value {
                [state] => Some(*state),
                _ => return Err(BAD),
            }
        }
        None => None,
    };
    if let Some(mech) = find(resp, 0xa1)? {
        if only(mech, 0x06)? != NTLMSSP_OID {
            return Err(Error::Refused(
                "the server chose a mechanism other than NTLM",
            ));
        }
    }
    let token = match find(resp, 0xa2)? {
        Some(field) => Some(only(field, 0x04)?),
        None => None,
    };
    Ok(Reply {
        wrapped: true,
        state,
        token,
    })
}

/// One DER element.
fn tlv(tag: u8, contents: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(contents.len() + 6);
    out.push(tag);
    let len = contents.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes = (len as u32).to_be_bytes();
        let skip = bytes.iter().take_while(|b| **b == 0).count();
        out.push(0x80 | (4 - skip) as u8);
        out.extend_from_slice(&bytes[skip..]);
    }
    out.extend_from_slice(contents);
    out
}

/// The client's first token: a `NegTokenInit` offering NTLMSSP alone, with
/// the NTLM NEGOTIATE inside.
pub fn wrap_init(ntlm: &[u8]) -> Vec<u8> {
    let types = tlv(0xa0, &tlv(0x30, &tlv(0x06, NTLMSSP_OID)));
    let token = tlv(0xa2, &tlv(0x04, ntlm));
    let mut init = types;
    init.extend_from_slice(&token);
    let body = tlv(0xa0, &tlv(0x30, &init));
    let mut inner = tlv(0x06, SPNEGO_OID);
    inner.extend_from_slice(&body);
    tlv(0x60, &inner)
}

/// The client's next token: a `NegTokenResp` carrying the NTLM AUTHENTICATE.
pub fn wrap_resp(ntlm: &[u8]) -> Vec<u8> {
    tlv(0xa1, &tlv(0x30, &tlv(0xa2, &tlv(0x04, ntlm))))
}
