//! The central directory and end-of-central-directory records.
//!
//! The EOCD is found by scanning backwards over at most 64 KiB + 22 bytes of
//! trailing comment, and only an EOCD whose declared comment length reaches the
//! end of the archive is accepted, so trailing data cannot fake one. Every
//! field read from a record is range-checked against the directory and the
//! archive before it is used.

use alloc::vec::Vec;

use crate::error::OpenError;
use crate::path;
use crate::zip::{
    extra_has_zip64, CentralDirectory, ZipEntry, CENTRAL_SIG, EOCD_SIG, ZIP64_LOCATOR_SIG,
};
use crate::{EntryInfo, MAX_ENTRIES, MAX_ENTRY_UNCOMPRESSED, MAX_NAME_LEN, MAX_TOTAL_UNCOMPRESSED};

/// Largest trailing comment an EOCD may declare (the zip format caps it here).
const MAX_COMMENT: usize = 0xFFFF;
/// Bytes of a central directory record before its variable-length fields.
const CENTRAL_HEADER: usize = 46;
/// Bytes of an end-of-central-directory record before its comment.
const EOCD_HEADER: usize = 22;

/// Find the offset of the last valid EOCD, scanning back over at most
/// 64 KiB + 22 bytes. Returns `None` when the archive is too short or holds no
/// record whose comment length reaches the end.
pub(crate) fn find_eocd(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < EOCD_HEADER {
        return None;
    }
    let lowest = bytes.len().saturating_sub(EOCD_HEADER + MAX_COMMENT);
    let mut at = bytes.len() - EOCD_HEADER;
    loop {
        if read_u32(bytes, at) == Some(EOCD_SIG) {
            let comment = read_u16(bytes, at + 20).unwrap_or(0) as usize;
            if at + EOCD_HEADER + comment == bytes.len() {
                return Some(at);
            }
        }
        if at == lowest {
            return None;
        }
        at -= 1;
    }
}

/// Parse and range-check the EOCD at `at`.
pub(crate) fn parse_eocd(bytes: &[u8], at: usize) -> Result<CentralDirectory, OpenError> {
    if read_u32(bytes, at) != Some(EOCD_SIG) {
        return Err(OpenError::NoEndOfCentralDirectory);
    }
    let disk = read_u16(bytes, at + 4).unwrap_or(0);
    let directory_disk = read_u16(bytes, at + 6).unwrap_or(0);
    let entries_disk = read_u16(bytes, at + 8).unwrap_or(0);
    let entries = read_u16(bytes, at + 10).unwrap_or(0);
    let size = read_u32(bytes, at + 12).unwrap_or(0);
    let offset = read_u32(bytes, at + 16).unwrap_or(0);

    if disk != 0 || directory_disk != 0 || entries_disk != entries {
        return Err(OpenError::MultiDiskUnsupported);
    }
    if entries == u16::MAX || size == u32::MAX || offset == u32::MAX {
        return Err(OpenError::Zip64Unsupported);
    }
    // The zip64 locator, when present, sits immediately before the EOCD.
    if at >= 20 && read_u32(bytes, at - 20) == Some(ZIP64_LOCATOR_SIG) {
        return Err(OpenError::Zip64Unsupported);
    }
    if u64::from(entries) > MAX_ENTRIES as u64 {
        return Err(OpenError::TooManyEntries {
            count: u64::from(entries),
        });
    }
    let end = (offset as usize)
        .checked_add(size as usize)
        .ok_or(OpenError::BadCentralDirectory)?;
    if end > at {
        return Err(OpenError::BadCentralDirectory);
    }
    Ok(CentralDirectory {
        offset: offset as usize,
        size: size as usize,
        entries: u64::from(entries),
    })
}

/// Walk `directory.entries` records and return the validated entries.
pub(crate) fn parse_directory<'a>(
    bytes: &'a [u8],
    directory: &CentralDirectory,
) -> Result<Vec<ZipEntry<'a>>, OpenError> {
    let end = directory
        .offset
        .checked_add(directory.size)
        .ok_or(OpenError::BadCentralDirectory)?;
    if end > bytes.len() {
        return Err(OpenError::BadCentralDirectory);
    }
    let mut at = directory.offset;
    let mut total: u64 = 0;
    let mut entries = Vec::new();
    for _ in 0..directory.entries {
        let (entry, next) = read_record(bytes, at, end, &mut total)?;
        entries.push(entry);
        at = next;
    }
    if at != end {
        return Err(OpenError::BadCentralDirectory);
    }
    Ok(entries)
}

/// Parse one central record at `at`, returning the entry and the offset of the
/// next record. `total` accumulates the claimed uncompressed size.
fn read_record<'a>(
    bytes: &'a [u8],
    at: usize,
    end: usize,
    total: &mut u64,
) -> Result<(ZipEntry<'a>, usize), OpenError> {
    let header_end = at
        .checked_add(CENTRAL_HEADER)
        .ok_or(OpenError::BadCentralRecord)?;
    if header_end > end {
        return Err(OpenError::BadCentralRecord);
    }
    if read_u32(bytes, at) != Some(CENTRAL_SIG) {
        return Err(OpenError::BadCentralRecord);
    }
    let flags = read_u16(bytes, at + 8).unwrap_or(0);
    let method = read_u16(bytes, at + 10).unwrap_or(0);
    let crc32 = read_u32(bytes, at + 16).unwrap_or(0);
    let compressed_size = read_u32(bytes, at + 20).unwrap_or(0);
    let size = read_u32(bytes, at + 24).unwrap_or(0);
    let name_len = read_u16(bytes, at + 28).unwrap_or(0) as usize;
    let extra_len = read_u16(bytes, at + 30).unwrap_or(0) as usize;
    let comment_len = read_u16(bytes, at + 32).unwrap_or(0) as usize;
    let disk_start = read_u16(bytes, at + 34).unwrap_or(0);
    let local_offset = read_u32(bytes, at + 42).unwrap_or(0);

    // zip64 uses sentinel widths; reject before any offset arithmetic.
    if size == u32::MAX
        || compressed_size == u32::MAX
        || local_offset == u32::MAX
        || disk_start == u16::MAX
    {
        return Err(OpenError::Zip64Unsupported);
    }
    if disk_start != 0 {
        return Err(OpenError::MultiDiskUnsupported);
    }

    let name_at = header_end;
    let name_end = name_at
        .checked_add(name_len)
        .ok_or(OpenError::BadCentralRecord)?;
    let extra_end = name_end
        .checked_add(extra_len)
        .ok_or(OpenError::BadCentralRecord)?;
    let next = extra_end
        .checked_add(comment_len)
        .ok_or(OpenError::BadCentralRecord)?;
    if next > end {
        return Err(OpenError::BadCentralRecord);
    }
    let name = core::str::from_utf8(&bytes[name_at..name_end]).map_err(|_| OpenError::BadName {
        reason: "is not valid UTF-8",
    })?;
    if name.len() > MAX_NAME_LEN {
        return Err(OpenError::BadName {
            reason: "is longer than 255 bytes",
        });
    }
    path::validate(name).map_err(|reason| OpenError::BadPath {
        name: name.into(),
        reason,
    })?;

    // Encryption (bit 0) and data descriptors (bit 3) change the record layout.
    if flags & 0x0001 != 0 || flags & 0x0008 != 0 {
        return Err(OpenError::UnsupportedFlags { name: name.into() });
    }
    if method != 0 && method != 8 {
        return Err(OpenError::UnsupportedCompression {
            name: name.into(),
            method,
        });
    }
    if extra_has_zip64(&bytes[name_end..extra_end]) {
        return Err(OpenError::Zip64Unsupported);
    }
    if u64::from(size) > u64::from(MAX_ENTRY_UNCOMPRESSED) {
        return Err(OpenError::EntryTooLarge {
            name: name.into(),
            size: u64::from(size),
        });
    }
    *total = total
        .checked_add(u64::from(size))
        .ok_or(OpenError::TotalTooLarge { total: u64::MAX })?;
    if *total > MAX_TOTAL_UNCOMPRESSED {
        return Err(OpenError::TotalTooLarge { total: *total });
    }

    let is_dir = name.ends_with('/');
    if is_dir && (size != 0 || compressed_size != 0) {
        return Err(OpenError::BadPath {
            name: name.into(),
            reason: "is a directory entry with data",
        });
    }
    let info = EntryInfo {
        name,
        size,
        compressed_size,
        crc32,
        is_dir,
    };
    Ok((
        ZipEntry {
            info,
            method,
            local_offset: local_offset as usize,
            data_start: 0,
        },
        next,
    ))
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    let slice = bytes.get(at..at + 2)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let slice = bytes.get(at..at + 4)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}
