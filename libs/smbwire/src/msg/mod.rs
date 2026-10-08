//! SMB2 command bodies (`MS-SMB2` 2.2): request encoders and response
//! decoders.
//!
//! Encoders return the body that follows the 64-byte header; every offset
//! they write counts from the start of the header, as the protocol does.
//! Decoders take the whole message (header included), check the response's
//! fixed `StructureSize`, and check every offset and length against the
//! message before a byte is read through it.

mod file;
mod info;
mod session;

pub use file::*;
pub use info::*;
pub use session::*;

use crate::header::SIZE as HEADER;
use crate::{le16, slice, Error};

/// Largest single READ, WRITE or listing the client asks for. Without the
/// `LARGE_MTU` capability (which the client does not request) SMB 2.1 caps
/// these at 64 KiB.
pub const MAX_IO: u32 = 64 * 1024;

/// A 16-byte file id (persistent and volatile halves).
pub type FileId = [u8; 16];

/// The response body after the header, if its `StructureSize` is `size`.
/// Odd sizes include the first byte of a variable part, which may be absent.
pub(crate) fn body<'a>(
    message: &'a [u8],
    size: u16,
    what: &'static str,
) -> Result<&'a [u8], Error> {
    let body = message.get(HEADER..).ok_or(Error::Malformed(what))?;
    if le16(body, 0) != Some(size) {
        return Err(Error::Malformed(what));
    }
    let need = (size as usize) & !1;
    if body.len() < need {
        return Err(Error::Malformed(what));
    }
    Ok(body)
}

/// `len` bytes at `offset` from the start of the message; an empty buffer
/// may have any offset.
pub(crate) fn buffer<'a>(
    message: &'a [u8],
    offset: usize,
    len: usize,
    what: &'static str,
) -> Result<&'a [u8], Error> {
    if len == 0 {
        return Ok(&[]);
    }
    if offset < HEADER {
        return Err(Error::Malformed(what));
    }
    slice(message, offset, len).ok_or(Error::Malformed(what))
}

/// The four-byte body of LOGOFF, TREE_DISCONNECT and ECHO requests.
pub fn empty_request() -> [u8; 4] {
    [4, 0, 0, 0]
}
