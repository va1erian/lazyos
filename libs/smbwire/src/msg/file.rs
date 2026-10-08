//! CREATE, CLOSE, FLUSH, READ and WRITE.

use alloc::vec::Vec;

use super::{body, buffer, FileId, MAX_IO};
use crate::header::SIZE as HEADER;
use crate::{le32, le64, Error};

/// Access rights (`MS-SMB2` 2.2.13.1).
pub mod access {
    pub const READ_DATA: u32 = 0x0000_0001;
    pub const LIST_DIRECTORY: u32 = 0x0000_0001;
    pub const WRITE_DATA: u32 = 0x0000_0002;
    pub const APPEND_DATA: u32 = 0x0000_0004;
    pub const READ_EA: u32 = 0x0000_0008;
    pub const READ_ATTRIBUTES: u32 = 0x0000_0080;
    pub const WRITE_ATTRIBUTES: u32 = 0x0000_0100;
    pub const DELETE: u32 = 0x0001_0000;
    pub const READ_CONTROL: u32 = 0x0002_0000;
    pub const SYNCHRONIZE: u32 = 0x0010_0000;
}

pub const SHARE_READ: u32 = 1;
pub const SHARE_WRITE: u32 = 2;
pub const SHARE_DELETE: u32 = 4;

/// `CreateDisposition`.
pub mod disposition {
    pub const SUPERSEDE: u32 = 0;
    pub const OPEN: u32 = 1;
    pub const CREATE: u32 = 2;
    pub const OPEN_IF: u32 = 3;
    pub const OVERWRITE: u32 = 4;
    pub const OVERWRITE_IF: u32 = 5;
}

pub const OPTION_DIRECTORY_FILE: u32 = 0x0000_0001;
pub const OPTION_NON_DIRECTORY_FILE: u32 = 0x0000_0040;

pub const ATTR_READONLY: u32 = 0x0000_0001;
pub const ATTR_HIDDEN: u32 = 0x0000_0002;
pub const ATTR_DIRECTORY: u32 = 0x0000_0010;
pub const ATTR_ARCHIVE: u32 = 0x0000_0020;
pub const ATTR_NORMAL: u32 = 0x0000_0080;

/// What CREATE asks for.
#[derive(Clone, Debug)]
pub struct CreateRequest {
    pub access: u32,
    pub attributes: u32,
    pub share: u32,
    pub disposition: u32,
    pub options: u32,
    /// The path, UTF-16LE, relative to the share (`crate::name::to_smb`).
    pub name: Vec<u8>,
}

/// The CREATE request body.
pub fn create_request(r: &CreateRequest) -> Result<Vec<u8>, Error> {
    let len = u16::try_from(r.name.len()).map_err(|_| Error::BadName)?;
    let mut out = Vec::with_capacity(56 + r.name.len().max(1));
    out.extend_from_slice(&57u16.to_le_bytes());
    out.push(0); // SecurityFlags
    out.push(0); // RequestedOplockLevel: none
    out.extend_from_slice(&2u32.to_le_bytes()); // Impersonation
    out.extend_from_slice(&[0; 16]); // SmbCreateFlags, Reserved
    out.extend_from_slice(&r.access.to_le_bytes());
    out.extend_from_slice(&r.attributes.to_le_bytes());
    out.extend_from_slice(&r.share.to_le_bytes());
    out.extend_from_slice(&r.disposition.to_le_bytes());
    out.extend_from_slice(&r.options.to_le_bytes());
    out.extend_from_slice(&((HEADER + 56) as u16).to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&[0; 8]); // no create contexts
    out.extend_from_slice(&r.name);
    if r.name.is_empty() {
        // The variable part is never empty on the wire.
        out.push(0);
    }
    Ok(out)
}

/// A file's times (FILETIME), sizes and attributes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileInfo {
    pub creation: u64,
    pub last_access: u64,
    pub last_write: u64,
    pub change: u64,
    pub allocation: u64,
    pub end_of_file: u64,
    pub attributes: u32,
}

impl FileInfo {
    pub fn is_dir(&self) -> bool {
        self.attributes & ATTR_DIRECTORY != 0
    }

    /// The four times, two sizes and attributes at `at` in `b` (the layout
    /// CREATE, CLOSE and FileNetworkOpenInformation share).
    pub(crate) fn read(b: &[u8], at: usize) -> Option<FileInfo> {
        Some(FileInfo {
            creation: le64(b, at)?,
            last_access: le64(b, at + 8)?,
            last_write: le64(b, at + 16)?,
            change: le64(b, at + 24)?,
            allocation: le64(b, at + 32)?,
            end_of_file: le64(b, at + 40)?,
            attributes: le32(b, at + 48)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateResponse {
    pub action: u32,
    pub info: FileInfo,
    pub file_id: FileId,
}

pub fn parse_create(message: &[u8]) -> Result<CreateResponse, Error> {
    const WHAT: &str = "CREATE response";
    let b = body(message, 89, WHAT)?;
    let bad = Error::Malformed(WHAT);
    let mut file_id = [0u8; 16];
    file_id.copy_from_slice(&b[64..80]);
    Ok(CreateResponse {
        action: le32(b, 4).ok_or(bad.clone())?,
        info: FileInfo::read(b, 8).ok_or(bad)?,
        file_id,
    })
}

/// CLOSE and FLUSH share a shape: size 24, two reserved fields, the id.
fn handle_request(file_id: &FileId) -> Vec<u8> {
    let mut out = Vec::with_capacity(24);
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 6]);
    out.extend_from_slice(file_id);
    out
}

pub fn close_request(file_id: &FileId) -> Vec<u8> {
    handle_request(file_id)
}

pub fn flush_request(file_id: &FileId) -> Vec<u8> {
    handle_request(file_id)
}

/// The READ request body for `len` bytes at `offset`.
pub fn read_request(file_id: &FileId, offset: u64, len: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(49);
    out.extend_from_slice(&49u16.to_le_bytes());
    out.push(0x50); // Padding: where the data should start in the response
    out.push(0);
    out.extend_from_slice(&len.min(MAX_IO).to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(file_id);
    out.extend_from_slice(&[0; 16]); // MinimumCount, Channel, RemainingBytes, channel info
    out.push(0);
    out
}

/// The data of a READ response; more than `asked` bytes is a protocol error.
pub fn parse_read(message: &[u8], asked: u32) -> Result<&[u8], Error> {
    const WHAT: &str = "READ response";
    let b = body(message, 17, WHAT)?;
    let len = le32(b, 4).ok_or(Error::Malformed(WHAT))?;
    if len > asked {
        return Err(Error::Malformed("READ returned more than was asked"));
    }
    buffer(message, b[2] as usize, len as usize, WHAT)
}

/// The WRITE request body for `data` at `offset`.
pub fn write_request(file_id: &FileId, offset: u64, data: &[u8]) -> Result<Vec<u8>, Error> {
    if data.len() > MAX_IO as usize {
        return Err(Error::Malformed("WRITE larger than the client's limit"));
    }
    let mut out = Vec::with_capacity(48 + data.len());
    out.extend_from_slice(&49u16.to_le_bytes());
    out.extend_from_slice(&((HEADER + 48) as u16).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(file_id);
    out.extend_from_slice(&[0; 16]); // Channel, RemainingBytes, channel info, Flags
    out.extend_from_slice(data);
    if data.is_empty() {
        out.push(0);
    }
    Ok(out)
}

/// The byte count of a WRITE response.
pub fn parse_write(message: &[u8]) -> Result<u32, Error> {
    const WHAT: &str = "WRITE response";
    let b = body(message, 17, WHAT)?;
    le32(b, 4).ok_or(Error::Malformed(WHAT))
}
