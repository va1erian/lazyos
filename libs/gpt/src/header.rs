//! The GPT header: 92 bytes at LBA 1 (primary) or the last LBA (backup).
//!
//! Every field that names a place on the disk is checked against the disk
//! and against the others, so nothing downstream reads outside the disk or
//! trusts an entry array that overlaps the partitions it describes.

use crate::crc::crc32;
use crate::guid::Guid;
use crate::{MAX_ARRAY_BYTES, SECTOR};

/// `EFI PART`.
pub const SIGNATURE: [u8; 8] = *b"EFI PART";
/// Revision 1.0, the only one there is.
pub const REVISION: u32 = 0x0001_0000;
/// The header's defined size; a larger one is allowed (zero padded).
pub const HEADER_SIZE: u32 = 92;
/// The smallest entry size; others are 128 times a power of two.
pub const ENTRY_SIZE: u32 = 128;

/// Why a header was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderError {
    Signature,
    Revision,
    Size,
    Crc,
    /// `my_lba` is not where the header was read.
    Location,
    /// An LBA field lies outside the disk, or the fields contradict.
    Bounds,
    /// Entry size or count unusable, or the array too large.
    Array,
}

/// A header that passed every check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub my_lba: u64,
    pub alternate_lba: u64,
    pub first_usable: u64,
    pub last_usable: u64,
    pub disk_guid: Guid,
    pub entries_lba: u64,
    pub entry_count: u32,
    pub entry_size: u32,
    pub entries_crc: u32,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from(u32_at(b, at)) | u64::from(u32_at(b, at + 4)) << 32
}

impl Header {
    /// Parse the header in `sector`, read from `lba` of a disk of
    /// `disk_sectors` sectors.
    pub fn parse(
        sector: &[u8; SECTOR],
        lba: u64,
        disk_sectors: u64,
    ) -> Result<Header, HeaderError> {
        if sector[..8] != SIGNATURE {
            return Err(HeaderError::Signature);
        }
        if u32_at(sector, 8) >> 16 != REVISION >> 16 {
            return Err(HeaderError::Revision);
        }
        let size = u32_at(sector, 12) as usize;
        if !(HEADER_SIZE as usize..=SECTOR).contains(&size) {
            return Err(HeaderError::Size);
        }
        let mut copy = [0u8; SECTOR];
        copy[..size].copy_from_slice(&sector[..size]);
        copy[16..20].fill(0);
        if crc32(&copy[..size]) != u32_at(sector, 16) {
            return Err(HeaderError::Crc);
        }
        let mut guid = [0u8; 16];
        guid.copy_from_slice(&sector[56..72]);
        let header = Header {
            my_lba: u64_at(sector, 24),
            alternate_lba: u64_at(sector, 32),
            first_usable: u64_at(sector, 40),
            last_usable: u64_at(sector, 48),
            disk_guid: Guid(guid),
            entries_lba: u64_at(sector, 72),
            entry_count: u32_at(sector, 80),
            entry_size: u32_at(sector, 84),
            entries_crc: u32_at(sector, 88),
        };
        if header.my_lba != lba {
            return Err(HeaderError::Location);
        }
        header.check(disk_sectors)?;
        Ok(header)
    }

    fn check(&self, disk_sectors: u64) -> Result<(), HeaderError> {
        let size = self.entry_size;
        let shape_ok = size >= ENTRY_SIZE
            && size.is_multiple_of(ENTRY_SIZE)
            && (size / ENTRY_SIZE).is_power_of_two();
        let bytes = u64::from(self.entry_count) * u64::from(size);
        if !shape_ok || self.entry_count == 0 || bytes > MAX_ARRAY_BYTES as u64 {
            return Err(HeaderError::Array);
        }
        // LBA 0 is the protective MBR; the usable area sits between the two
        // headers and must not hold either one.
        let in_disk = |lba: u64| lba >= 1 && lba < disk_sectors;
        // Each header names the other: the backup points back at LBA 1, and
        // the primary's backup lies past its usable area. The primary's need
        // not be the disk's last sector: a disk grown after partitioning
        // keeps its backup at the old end, and must still read.
        let alternate_ok = match self.my_lba {
            1 => self.alternate_lba > self.last_usable,
            _ => self.alternate_lba == 1,
        };
        let usable_ok = self.first_usable <= self.last_usable
            && self.first_usable >= 2
            && in_disk(self.last_usable)
            && !(self.first_usable..=self.last_usable).contains(&self.my_lba)
            && self.alternate_lba != self.my_lba
            && alternate_ok
            && in_disk(self.alternate_lba);
        if !usable_ok {
            return Err(HeaderError::Bounds);
        }
        // The array is wholly inside the disk, clear of the usable area and
        // of this header.
        let array_end = self
            .entries_lba
            .checked_add(self.array_sectors())
            .ok_or(HeaderError::Bounds)?;
        let array_ok = self.entries_lba >= 1
            && array_end <= disk_sectors
            && (array_end <= self.first_usable || self.entries_lba > self.last_usable)
            && !(self.entries_lba..array_end).contains(&self.my_lba);
        if !array_ok {
            return Err(HeaderError::Bounds);
        }
        Ok(())
    }

    /// The entry array's size in bytes (what its CRC covers).
    pub fn array_bytes(&self) -> usize {
        self.entry_count as usize * self.entry_size as usize
    }

    /// The sectors the entry array occupies.
    pub fn array_sectors(&self) -> u64 {
        (self.array_bytes() as u64).div_ceil(SECTOR as u64)
    }

    /// The header as one sector, its CRC computed (the writer's inverse of
    /// [`Header::parse`]).
    pub fn to_sector(&self) -> [u8; SECTOR] {
        let mut s = [0u8; SECTOR];
        s[..8].copy_from_slice(&SIGNATURE);
        s[8..12].copy_from_slice(&REVISION.to_le_bytes());
        s[12..16].copy_from_slice(&HEADER_SIZE.to_le_bytes());
        s[24..32].copy_from_slice(&self.my_lba.to_le_bytes());
        s[32..40].copy_from_slice(&self.alternate_lba.to_le_bytes());
        s[40..48].copy_from_slice(&self.first_usable.to_le_bytes());
        s[48..56].copy_from_slice(&self.last_usable.to_le_bytes());
        s[56..72].copy_from_slice(&self.disk_guid.0);
        s[72..80].copy_from_slice(&self.entries_lba.to_le_bytes());
        s[80..84].copy_from_slice(&self.entry_count.to_le_bytes());
        s[84..88].copy_from_slice(&self.entry_size.to_le_bytes());
        s[88..92].copy_from_slice(&self.entries_crc.to_le_bytes());
        let crc = crc32(&s[..HEADER_SIZE as usize]);
        s[16..20].copy_from_slice(&crc.to_le_bytes());
        s
    }
}
