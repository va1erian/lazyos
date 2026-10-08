//! Direct-TCP framing (`MS-SMB2` 2.1): every message on port 445 is preceded
//! by a zero byte and a 24-bit big-endian length.
//!
//! [`FrameReader`] takes bytes in any chunking and returns whole frames. The
//! length is bounded by [`MAX_FRAME`] before a byte of the body is buffered,
//! so a hostile length cannot make the client allocate.

use alloc::vec::Vec;

use crate::Error;

/// Largest frame accepted: one 64 KiB read or write plus its header, with
/// room for a directory listing of the same size. The client never asks for
/// more than [`crate::msg::MAX_IO`] per request.
pub const MAX_FRAME: usize = 256 * 1024;

/// `message` with its transport header.
pub fn encode(message: &[u8]) -> Result<Vec<u8>, Error> {
    if message.len() > 0x00FF_FFFF {
        return Err(Error::Malformed("message too long to frame"));
    }
    let len = message.len() as u32;
    let mut out = Vec::with_capacity(4 + message.len());
    out.push(0);
    out.extend_from_slice(&len.to_be_bytes()[1..]);
    out.extend_from_slice(message);
    Ok(out)
}

/// Reassembles frames from a byte stream.
#[derive(Default)]
pub struct FrameReader {
    buffer: Vec<u8>,
}

impl FrameReader {
    pub fn new() -> FrameReader {
        FrameReader::default()
    }

    /// Append received bytes. A frame header that cannot be valid fails at
    /// once (and the connection must be dropped).
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.buffer.extend_from_slice(bytes);
        self.check_header()
    }

    fn check_header(&self) -> Result<(), Error> {
        if let Some(&kind) = self.buffer.first() {
            // 0x00 is a session message; NetBIOS keep-alives (0x85) and the
            // rest of RFC 1002 never appear on a Direct-TCP connection.
            if kind != 0 {
                return Err(Error::Malformed("not a session message"));
            }
        }
        if let Some(len) = self.declared() {
            if len > MAX_FRAME {
                return Err(Error::Malformed("frame too long"));
            }
            if len < crate::header::SIZE {
                return Err(Error::Malformed("frame shorter than a header"));
            }
        }
        Ok(())
    }

    fn declared(&self) -> Option<usize> {
        let head = self.buffer.get(..4)?;
        Some(u32::from_be_bytes([0, head[1], head[2], head[3]]) as usize)
    }

    /// The next whole frame's body, if one has arrived.
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        let len = self.declared()?;
        if self.buffer.len() < 4 + len {
            return None;
        }
        let body = self.buffer[4..4 + len].to_vec();
        self.buffer.drain(..4 + len);
        Some(body)
    }

    /// Bytes held that do not yet make a frame.
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }
}
