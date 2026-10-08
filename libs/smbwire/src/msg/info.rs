//! QUERY_DIRECTORY, QUERY_INFO and SET_INFO, and the information classes the
//! client reads and writes.

use alloc::string::String;
use alloc::vec::Vec;

use super::{body, buffer, FileId, FileInfo};
use crate::header::SIZE as HEADER;
use crate::{le16, le32, le64, slice, Error};

pub const INFO_FILE: u8 = 1;
pub const INFO_FILESYSTEM: u8 = 2;

/// File information classes (`MS-FSCC` 2.4).
pub const CLASS_RENAME: u8 = 10;
pub const CLASS_DISPOSITION: u8 = 13;
pub const CLASS_END_OF_FILE: u8 = 20;
pub const CLASS_NETWORK_OPEN: u8 = 34;
pub const CLASS_ID_BOTH_DIRECTORY: u8 = 37;
/// File system class: FileFsFullSizeInformation.
pub const CLASS_FS_FULL_SIZE: u8 = 7;

pub const QUERY_RESTART_SCANS: u8 = 0x01;

/// The QUERY_DIRECTORY request body.
pub fn query_directory_request(
    file_id: &FileId,
    class: u8,
    flags: u8,
    pattern: &[u8],
    output: u32,
) -> Result<Vec<u8>, Error> {
    let len = u16::try_from(pattern.len()).map_err(|_| Error::BadName)?;
    let mut out = Vec::with_capacity(32 + pattern.len());
    out.extend_from_slice(&33u16.to_le_bytes());
    out.push(class);
    out.push(flags);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(file_id);
    out.extend_from_slice(&((HEADER + 32) as u16).to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&output.to_le_bytes());
    out.extend_from_slice(pattern);
    if pattern.is_empty() {
        out.push(0);
    }
    Ok(out)
}

/// The output buffer of a QUERY_DIRECTORY or QUERY_INFO response (both are
/// `StructureSize` 9 with the offset and length in the same place); more
/// than `asked` bytes is a protocol error.
pub fn parse_output(message: &[u8], asked: u32) -> Result<&[u8], Error> {
    const WHAT: &str = "query response";
    let b = body(message, 9, WHAT)?;
    let offset = le16(b, 2).ok_or(Error::Malformed(WHAT))? as usize;
    let len = le32(b, 4).ok_or(Error::Malformed(WHAT))?;
    if len > asked {
        return Err(Error::Malformed("query returned more than was asked"));
    }
    buffer(message, offset, len as usize, WHAT)
}

/// One FileIdBothDirectoryInformation entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub info: FileInfo,
    pub file_id: u64,
}

/// The fixed part of a FileIdBothDirectoryInformation entry.
const ENTRY_FIXED: usize = 104;
/// Most entries one buffer may hold (a 64 KiB buffer of minimal entries).
const MAX_ENTRIES: usize = 64 * 1024 / ENTRY_FIXED + 1;

/// The entries of a FileIdBothDirectoryInformation buffer, and how many were
/// skipped (`.`, `..`, and names the client cannot address).
pub fn parse_directory(buffer: &[u8]) -> Result<(Vec<DirEntry>, usize), Error> {
    let bad = Error::Malformed("directory entry");
    let (mut entries, mut skipped, mut at) = (Vec::new(), 0usize, 0usize);
    if buffer.is_empty() {
        return Ok((entries, skipped));
    }
    for _ in 0..MAX_ENTRIES {
        let entry = buffer.get(at..).ok_or(bad.clone())?;
        let next = le32(entry, 0).ok_or(bad.clone())? as usize;
        let name_len = le32(entry, 60).ok_or(bad.clone())? as usize;
        let name = slice(entry, ENTRY_FIXED, name_len).ok_or(bad.clone())?;
        if next != 0 && next < ENTRY_FIXED + name_len {
            return Err(bad);
        }
        let info = FileInfo {
            creation: le64(entry, 8).ok_or(bad.clone())?,
            last_access: le64(entry, 16).ok_or(bad.clone())?,
            last_write: le64(entry, 24).ok_or(bad.clone())?,
            change: le64(entry, 32).ok_or(bad.clone())?,
            end_of_file: le64(entry, 40).ok_or(bad.clone())?,
            allocation: le64(entry, 48).ok_or(bad.clone())?,
            attributes: le32(entry, 56).ok_or(bad.clone())?,
        };
        let file_id = le64(entry, 96).ok_or(bad.clone())?;
        match crate::name::listed(name) {
            Some(name) => entries.push(DirEntry {
                name,
                info,
                file_id,
            }),
            None => skipped += 1,
        }
        if next == 0 {
            return Ok((entries, skipped));
        }
        at = at.checked_add(next).ok_or(bad.clone())?;
    }
    Err(bad)
}

/// The QUERY_INFO request body (no input buffer).
pub fn query_info_request(file_id: &FileId, info_type: u8, class: u8, output: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(41);
    out.extend_from_slice(&41u16.to_le_bytes());
    out.push(info_type);
    out.push(class);
    out.extend_from_slice(&output.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // InputBufferOffset
    out.extend_from_slice(&[0; 2]);
    out.extend_from_slice(&0u32.to_le_bytes()); // InputBufferLength
    out.extend_from_slice(&0u32.to_le_bytes()); // AdditionalInformation
    out.extend_from_slice(&0u32.to_le_bytes()); // Flags
    out.extend_from_slice(file_id);
    out.push(0);
    out
}

/// FileNetworkOpenInformation.
pub fn parse_network_open(buffer: &[u8]) -> Result<FileInfo, Error> {
    if buffer.len() < 56 {
        return Err(Error::Malformed("FileNetworkOpenInformation"));
    }
    FileInfo::read(buffer, 0).ok_or(Error::Malformed("FileNetworkOpenInformation"))
}

/// FileFsFullSizeInformation, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FsSize {
    pub total: u64,
    pub available: u64,
    pub unit: u64,
}

pub fn parse_fs_full_size(buffer: &[u8]) -> Result<FsSize, Error> {
    let bad = Error::Malformed("FileFsFullSizeInformation");
    let total = le64(buffer, 0).ok_or(bad.clone())?;
    let available = le64(buffer, 8).ok_or(bad.clone())?;
    let sectors = le32(buffer, 24).ok_or(bad.clone())? as u64;
    let bytes = le32(buffer, 28).ok_or(bad.clone())? as u64;
    let unit = sectors
        .checked_mul(bytes)
        .filter(|u| *u != 0)
        .ok_or(bad.clone())?;
    Ok(FsSize {
        total: total.checked_mul(unit).ok_or(bad.clone())?,
        available: available.checked_mul(unit).ok_or(bad)?,
        unit,
    })
}

/// The SET_INFO request body.
pub fn set_info_request(
    file_id: &FileId,
    info_type: u8,
    class: u8,
    data: &[u8],
) -> Result<Vec<u8>, Error> {
    let len = u32::try_from(data.len()).map_err(|_| Error::BadName)?;
    let mut out = Vec::with_capacity(32 + data.len());
    out.extend_from_slice(&33u16.to_le_bytes());
    out.push(info_type);
    out.push(class);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&((HEADER + 32) as u16).to_le_bytes());
    out.extend_from_slice(&[0; 2]);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(file_id);
    out.extend_from_slice(data);
    Ok(out)
}

/// FileRenameInformation (the SMB2 form): the new name is a path from the
/// share root, UTF-16LE.
pub fn rename_info(replace: bool, new_name: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(20 + new_name.len());
    out.push(replace as u8);
    out.extend_from_slice(&[0; 7]);
    out.extend_from_slice(&0u64.to_le_bytes()); // RootDirectory
    out.extend_from_slice(&(new_name.len() as u32).to_le_bytes());
    out.extend_from_slice(new_name);
    out
}

/// FileDispositionInformation: delete on close.
pub fn disposition_info() -> [u8; 1] {
    [1]
}

/// FileEndOfFileInformation.
pub fn end_of_file_info(size: u64) -> [u8; 8] {
    size.to_le_bytes()
}
