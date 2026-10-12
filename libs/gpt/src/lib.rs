//! GUID Partition Tables (UEFI 2.10 section 5.3) for the kernel's block layer
//! and `lazyos-install` (docs/grub-install-plan.md K2).
//!
//! A partition table is hostile input. [`read`] takes the primary header at
//! LBA 1 and its entry array, both CRC-checked and bounds-checked against the
//! disk, and falls back to the backup header in the last sector when anything
//! about the primary is wrong. Entries are validated one by one: an entry is
//! used only when it lies wholly inside the usable area and overlaps no other
//! entry; a bad one is reported and dropped, never clamped.
//!
//! * [`guid`]: the mixed-endian GUID form and the partition types LazyOS knows.
//! * [`header`]: the 92-byte header and its rules.
//! * [`entry`]: one entry of the array and the array's rules.
//! * [`crc`]: the CRC-32 GPT uses (IEEE 802.3, reflected).
//! * [`write`]: building a table (tests, the kernel suite, the installer's
//!   harness), with the same rules [`read`] enforces.
//!
//! `no_std` with `alloc`: the entry array is read into a `Vec`.

#![no_std]

extern crate alloc;
#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod crc;
pub mod entry;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod guid;
pub mod header;
#[cfg(test)]
mod tests;
pub mod write;

use alloc::vec;
use alloc::vec::Vec;

pub use entry::{Entry, EntryError};
pub use guid::Guid;
pub use header::{Header, HeaderError};

/// The only logical sector size the reader serves (as the block layer).
pub const SECTOR: usize = 512;

/// The largest entry array the reader accepts, in bytes (1024 entries of 128
/// bytes): far beyond any real table, small enough to read at boot.
pub const MAX_ARRAY_BYTES: usize = 128 * 1024;

/// Where the table that was used came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Primary,
    Backup,
}

/// Why one copy of the table could not be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyError {
    /// The device refused the read.
    Io,
    Header(HeaderError),
    /// The entry array's CRC does not match the header's.
    ArrayCrc,
}

/// A table that read: the header, the entries that passed, and the ones that
/// did not (by 1-based index).
#[derive(Clone, Debug)]
pub struct Table {
    pub source: Source,
    pub header: Header,
    pub entries: Vec<Entry>,
    pub rejected: Vec<(u32, EntryError)>,
    /// Why the primary copy was not used, when the backup was.
    pub primary_error: Option<CopyError>,
}

impl Table {
    /// The used entry named `name` (the GPT partition name), when exactly one
    /// has it.
    pub fn by_name(&self, name: &str) -> Option<&Entry> {
        let mut found = self.entries.iter().filter(|entry| entry.name_is(name));
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }
}

/// Neither copy is usable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failure {
    pub primary: CopyError,
    pub backup: CopyError,
}

/// Whole-sector reads from the disk being parsed.
pub trait Reader {
    /// Fill `buf` (a multiple of [`SECTOR`]) from `lba`; `false` on any error.
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool;
}

/// Whether `mbr` (sector 0) is a protective or hybrid MBR: signed, with an
/// entry of type `0xEE`. Such a disk is read as GPT.
pub fn is_protective(mbr: &[u8; SECTOR]) -> bool {
    mbr[510..] == [0x55, 0xAA] && (0..4).any(|slot| mbr[446 + slot * 16 + 4] == 0xEE)
}

/// Read the table of a disk of `disk_sectors` 512-byte sectors: the primary
/// copy, else the backup in the last sector.
pub fn read(reader: &mut impl Reader, disk_sectors: u64) -> Result<Table, Failure> {
    let primary = match read_copy(reader, 1, disk_sectors) {
        Ok((header, array)) => return Ok(table(Source::Primary, header, &array, None)),
        Err(error) => error,
    };
    let last = disk_sectors.saturating_sub(1);
    match read_copy(reader, last, disk_sectors) {
        Ok((header, array)) => Ok(table(Source::Backup, header, &array, Some(primary))),
        Err(backup) => Err(Failure { primary, backup }),
    }
}

fn read_copy(
    reader: &mut impl Reader,
    lba: u64,
    disk_sectors: u64,
) -> Result<(Header, Vec<u8>), CopyError> {
    if lba < 1 || lba >= disk_sectors {
        return Err(CopyError::Header(HeaderError::Bounds));
    }
    let mut sector = [0u8; SECTOR];
    if !reader.read(lba, &mut sector) {
        return Err(CopyError::Io);
    }
    let header = Header::parse(&sector, lba, disk_sectors).map_err(CopyError::Header)?;
    let mut array = vec![0u8; header.array_sectors() as usize * SECTOR];
    if !reader.read(header.entries_lba, &mut array) {
        return Err(CopyError::Io);
    }
    if crc::crc32(&array[..header.array_bytes()]) != header.entries_crc {
        return Err(CopyError::ArrayCrc);
    }
    Ok((header, array))
}

fn table(source: Source, header: Header, array: &[u8], primary_error: Option<CopyError>) -> Table {
    let (entries, rejected) = entry::validate(&header, array);
    Table {
        source,
        header,
        entries,
        rejected,
        primary_error,
    }
}
