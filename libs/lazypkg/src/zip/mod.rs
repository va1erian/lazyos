//! Zip container parsing.
//!
//! Only the subset LazyOS packages use is accepted, and everything is checked
//! against the archive length before it is used: one end-of-central-directory
//! record, a central directory of at most [`crate::MAX_ENTRIES`] entries, and a
//! matching local header per entry. Zip64, multi-disk archives, encryption and
//! data descriptors are refused rather than guessed at. See
//! [`central`] and [`local`] for the record-level checks.

pub(crate) mod central;
pub(crate) mod local;

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use crate::error::OpenError;

/// Local file header signature (`PK\x03\x04`).
pub(crate) const LOCAL_SIG: u32 = 0x0403_4b50;
/// Central directory file header signature (`PK\x01\x02`).
pub(crate) const CENTRAL_SIG: u32 = 0x0201_4b50;
/// End-of-central-directory signature (`PK\x05\x06`).
pub(crate) const EOCD_SIG: u32 = 0x0605_4b50;
/// Zip64 end-of-central-directory locator signature (`PK\x06\x07`).
pub(crate) const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;
/// Extra-field header id for the zip64 extended information record.
pub(crate) const ZIP64_EXTRA_ID: u16 = 0x0001;

/// A public, borrowed description of one archive entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryInfo<'a> {
    /// The entry name, already validated (relative, UTF-8, no escapes).
    pub name: &'a str,
    /// Uncompressed size in bytes.
    pub size: u32,
    /// Compressed size in bytes.
    pub compressed_size: u32,
    /// CRC-32 of the uncompressed bytes.
    pub crc32: u32,
    /// Whether the entry is a plain directory (name ends in `/`).
    pub is_dir: bool,
}

/// A validated central directory entry plus the resolved data offset.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ZipEntry<'a> {
    pub(crate) info: EntryInfo<'a>,
    pub(crate) method: u16,
    /// Offset of the local header, from the central record.
    pub(crate) local_offset: usize,
    /// Offset of the first compressed byte, resolved from the local header.
    pub(crate) data_start: usize,
}

/// Parse the whole container: locate the EOCD, walk the central directory,
/// resolve each local header, then reject duplicate and case-colliding names.
pub(crate) fn parse(bytes: &[u8]) -> Result<Vec<ZipEntry<'_>>, OpenError> {
    let eocd = central::find_eocd(bytes).ok_or(OpenError::NoEndOfCentralDirectory)?;
    let directory = central::parse_eocd(bytes, eocd)?;
    let mut entries = central::parse_directory(bytes, &directory)?;
    for entry in &mut entries {
        entry.data_start = local::data_start(bytes, entry, directory.offset)?;
    }
    check_names(&entries)?;
    Ok(entries)
}

/// Reject exact duplicates and names that differ only in case, because the
/// target filesystem may fold case and two such entries would collide there.
fn check_names(entries: &[ZipEntry<'_>]) -> Result<(), OpenError> {
    let mut exact: BTreeSet<&str> = BTreeSet::new();
    let mut folded: BTreeSet<alloc::string::String> = BTreeSet::new();
    for entry in entries {
        let name = entry.info.name;
        if !exact.insert(name) {
            return Err(OpenError::DuplicateName { name: name.into() });
        }
        if !folded.insert(name.to_lowercase()) {
            return Err(OpenError::CaseCollision { name: name.into() });
        }
    }
    Ok(())
}

/// Whether `extra` (an extra-field block) contains a zip64 record.
pub(crate) fn extra_has_zip64(extra: &[u8]) -> bool {
    let mut at = 0usize;
    while at + 4 <= extra.len() {
        let id = u16::from_le_bytes([extra[at], extra[at + 1]]);
        let size = usize::from(u16::from_le_bytes([extra[at + 2], extra[at + 3]]));
        if id == ZIP64_EXTRA_ID {
            return true;
        }
        match at.checked_add(4).and_then(|start| start.checked_add(size)) {
            Some(next) => at = next,
            None => break,
        }
    }
    false
}

/// The entry-count and offset bounds from the EOCD, already range-checked.
pub(crate) struct CentralDirectory {
    pub(crate) offset: usize,
    pub(crate) size: usize,
    pub(crate) entries: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn zip64_extra_is_detected() {
        // id 0x0001, size 8, then eight payload bytes.
        let mut extra = vec![0x01, 0x00, 0x08, 0x00];
        extra.extend_from_slice(&[0u8; 8]);
        assert!(extra_has_zip64(&extra));

        // id 0x5455 (extended timestamp), size 1.
        let other = vec![0x55, 0x54, 0x01, 0x00, 0x00];
        assert!(!extra_has_zip64(&other));
        assert!(!extra_has_zip64(&[]));
        // Truncated header is not a zip64 record and must not panic.
        assert!(!extra_has_zip64(&[0x01]));
    }
}
