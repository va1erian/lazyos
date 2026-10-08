//! The SMB 2.1 client protocol (docs/smb-plan.md §4, stage F2).
//!
//! Everything a file server (or a hostile network) can influence lives here,
//! pure and host-tested, with no I/O of its own:
//!
//! * [`frame`]: the Direct-TCP framing on port 445 (a 4-byte length prefix),
//!   bounded, fed bytes in any chunking.
//! * [`header`]: the 64-byte SMB2 header.
//! * [`msg`]: the request encoders and response decoders of every command the
//!   client sends, each response checked against its fixed structure size and
//!   every offset and length checked against the message before it is used.
//! * [`ntlm`] and [`spnego`]: NTLMv2 authentication (`MS-NLMP`), raw or inside
//!   the SPNEGO wrapper Samba and Windows use.
//! * [`crypto`]: MD4/MD5/HMAC for NTLMv2 and the HMAC-SHA256 message
//!   signature of SMB 2.x.
//! * [`client`]: a synchronous session over a [`client::Transport`] (the
//!   native daemon and command pass a TCP stream; tests pass a script): the
//!   negotiate, the logon, tree connects and file operations, with message ids,
//!   credits, signing and the guest/encryption refusals in one place.
//!
//! The library holds no clock and no random source: the caller supplies the
//! client challenge, the `ClientGuid` and the time ([`client::Config`]).

#![cfg_attr(not(any(test, feature = "fuzz")), no_std)]

extern crate alloc;

pub mod client;
pub mod crypto;
pub mod frame;
pub mod header;
pub mod msg;
pub mod name;
pub mod ntlm;
pub mod spnego;
pub mod status;

#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;

#[cfg(test)]
mod tests;

use alloc::string::String;

/// Why an operation failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The transport failed (the caller's words).
    Transport(String),
    /// The server closed the connection.
    Closed,
    /// A message broke the protocol; the text says where.
    Malformed(&'static str),
    /// The server answered a command with an error status.
    Status { command: u16, status: u32 },
    /// The server and the client share no dialect.
    Dialect(u16),
    /// A signed message's signature is wrong, or a message that must be signed
    /// is not.
    Signature,
    /// The server requires something this client will not do; the text says
    /// what (encryption, signing, guest logon).
    Refused(&'static str),
    /// A path or name the caller gave cannot be sent.
    BadName,
    /// The server granted no credit for the next request.
    NoCredits,
}

impl Error {
    /// The status of a server refusal, if this is one.
    pub fn status(&self) -> Option<u32> {
        match self {
            Error::Status { status, .. } => Some(*status),
            _ => None,
        }
    }
}

/// Read a little-endian `u16` at `at`, if the bytes are there.
pub(crate) fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

/// Read a little-endian `u32` at `at`, if the bytes are there.
pub(crate) fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// Read a little-endian `u64` at `at`, if the bytes are there.
pub(crate) fn le64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

/// `len` bytes at `offset` within `bytes`, if they are all there.
pub(crate) fn slice(bytes: &[u8], offset: usize, len: usize) -> Option<&[u8]> {
    bytes.get(offset..offset.checked_add(len)?)
}
