//! Parcel header, shared-buffer descriptors, and the parcel-level codec.

use alloc::vec::Vec;

use crate::tlv::{read_u16, read_u32, read_u64};
use crate::{
    Error, BUFFER_DESC_SIZE, HEADER_SIZE, MAX_BODY_BYTES, MAX_BUFFERS, MAX_HANDLES,
    MAX_PARCEL_BYTES, VERSION,
};

/// Fixed header of a parcel.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Header {
    pub version: u16,
    pub flags: u16,
    pub interface_id: u64,
    pub method: u32,
    pub txn_id: u64,
    pub reply_to: u64,
    pub deadline_ns: u64,
}

/// A shared-buffer descriptor carried by a parcel.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct BufferDesc {
    pub handle: u64,
    pub offset: u64,
    pub len: u64,
    pub flags: u32,
}

/// A complete message: header, TLV body, transferred handles, shared buffers.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Parcel {
    pub header: Header,
    pub body: Vec<u8>,
    pub handles: Vec<u64>,
    pub buffers: Vec<BufferDesc>,
}

impl Parcel {
    /// Encode into `out`, replacing its contents.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let body_len = self.body.len();
        if body_len > MAX_BODY_BYTES
            || self.handles.len() > MAX_HANDLES
            || self.buffers.len() > MAX_BUFFERS
        {
            return Err(Error::TooLarge);
        }
        let total = HEADER_SIZE
            .checked_add(body_len)
            .and_then(|n| n.checked_add(self.handles.len() * 8))
            .and_then(|n| n.checked_add(self.buffers.len() * BUFFER_DESC_SIZE))
            .ok_or(Error::TooLarge)?;
        if total > MAX_PARCEL_BYTES {
            return Err(Error::TooLarge);
        }

        out.clear();
        out.reserve(total);
        let h = &self.header;
        out.extend_from_slice(&h.version.to_le_bytes());
        out.extend_from_slice(&h.flags.to_le_bytes());
        out.extend_from_slice(&h.interface_id.to_le_bytes());
        out.extend_from_slice(&h.method.to_le_bytes());
        out.extend_from_slice(&h.txn_id.to_le_bytes());
        out.extend_from_slice(&h.reply_to.to_le_bytes());
        out.extend_from_slice(&h.deadline_ns.to_le_bytes());
        out.extend_from_slice(&(body_len as u32).to_le_bytes());
        out.extend_from_slice(&(self.handles.len() as u16).to_le_bytes());
        out.extend_from_slice(&(self.buffers.len() as u16).to_le_bytes());
        debug_assert_eq!(out.len(), HEADER_SIZE);

        out.extend_from_slice(&self.body);
        for handle in &self.handles {
            out.extend_from_slice(&handle.to_le_bytes());
        }
        for buffer in &self.buffers {
            out.extend_from_slice(&buffer.handle.to_le_bytes());
            out.extend_from_slice(&buffer.offset.to_le_bytes());
            out.extend_from_slice(&buffer.len.to_le_bytes());
            out.extend_from_slice(&buffer.flags.to_le_bytes());
        }
        Ok(())
    }

    /// Decode a parcel, validating every length and limit.
    pub fn decode(bytes: &[u8]) -> Result<Parcel, Error> {
        if bytes.len() < HEADER_SIZE {
            return Err(Error::Truncated);
        }
        if bytes.len() > MAX_PARCEL_BYTES {
            return Err(Error::TooLarge);
        }
        let version = read_u16(bytes, 0)?;
        if version != VERSION {
            return Err(Error::BadVersion);
        }
        let header = Header {
            version,
            flags: read_u16(bytes, 2)?,
            interface_id: read_u64(bytes, 4)?,
            method: read_u32(bytes, 12)?,
            txn_id: read_u64(bytes, 16)?,
            reply_to: read_u64(bytes, 24)?,
            deadline_ns: read_u64(bytes, 32)?,
        };
        let body_len = read_u32(bytes, 40)? as usize;
        let handle_count = read_u16(bytes, 44)? as usize;
        let buffer_count = read_u16(bytes, 46)? as usize;
        if body_len > MAX_BODY_BYTES || handle_count > MAX_HANDLES || buffer_count > MAX_BUFFERS {
            return Err(Error::TooLarge);
        }

        let handles_at = HEADER_SIZE + body_len;
        let buffers_at = handles_at + handle_count * 8;
        let end = buffers_at + buffer_count * BUFFER_DESC_SIZE;
        if end > bytes.len() {
            return Err(Error::Truncated);
        }
        if end != bytes.len() {
            return Err(Error::TrailingBytes);
        }

        let body = bytes[HEADER_SIZE..handles_at].to_vec();
        let mut handles = Vec::with_capacity(handle_count);
        for i in 0..handle_count {
            handles.push(read_u64(bytes, handles_at + i * 8)?);
        }
        let mut buffers = Vec::with_capacity(buffer_count);
        for i in 0..buffer_count {
            let at = buffers_at + i * BUFFER_DESC_SIZE;
            buffers.push(BufferDesc {
                handle: read_u64(bytes, at)?,
                offset: read_u64(bytes, at + 8)?,
                len: read_u64(bytes, at + 16)?,
                flags: read_u32(bytes, at + 24)?,
            });
        }
        Ok(Parcel {
            header,
            body,
            handles,
            buffers,
        })
    }
}
