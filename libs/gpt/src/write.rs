//! Building a GPT: the protective MBR, both headers and both entry arrays in
//! the standard places (128 entries of 128 bytes, usable area from LBA 34 to
//! 34 sectors before the end). Used by host tests, the kernel suite and the
//! installer's harness; what it writes passes [`crate::read`].

use alloc::vec;
use alloc::vec::Vec;

use crate::crc::crc32;
use crate::entry::{Entry, NAME_UNITS};
use crate::guid::Guid;
use crate::header::{Header, ENTRY_SIZE};
use crate::SECTOR;

/// Entries in a table this module writes.
pub const ENTRIES: u32 = 128;
/// Sectors of the entry array (16 KiB).
pub const ARRAY_SECTORS: u64 = (ENTRIES as u64 * ENTRY_SIZE as u64) / SECTOR as u64;
/// The first usable LBA: MBR, header, array.
pub const FIRST_USABLE: u64 = 2 + ARRAY_SECTORS;

/// One partition to write.
#[derive(Clone, Copy, Debug)]
pub struct Spec<'a> {
    pub type_guid: Guid,
    pub unique_guid: Guid,
    pub first_lba: u64,
    /// Inclusive.
    pub last_lba: u64,
    pub name: &'a str,
}

/// What [`build`] refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildError {
    DiskTooSmall,
    TooManyPartitions,
    /// Partition `n` (1-based) leaves the usable area or is inverted.
    Extent(u32),
    /// Partition `n` overlaps an earlier one.
    Overlap(u32),
    /// Partition `n`'s name is longer than 36 UTF-16 units.
    Name(u32),
    /// Partition `n` has the zero type GUID, which means "unused".
    Type(u32),
}

/// The sectors to write: `head` at LBA 0, `tail` at `tail_lba`.
#[derive(Clone, Debug)]
pub struct Layout {
    pub head: Vec<u8>,
    pub tail_lba: u64,
    pub tail: Vec<u8>,
}

impl Layout {
    /// Write both parts into a whole-disk image held in memory.
    pub fn apply(&self, disk: &mut [u8]) {
        disk[..self.head.len()].copy_from_slice(&self.head);
        let at = self.tail_lba as usize * SECTOR;
        disk[at..at + self.tail.len()].copy_from_slice(&self.tail);
    }
}

/// The last usable LBA of a disk of `disk_sectors`.
pub fn last_usable(disk_sectors: u64) -> u64 {
    disk_sectors - 2 - ARRAY_SECTORS
}

/// The table for `parts` (index = position + 1) on a disk of `disk_sectors`.
pub fn build(disk_sectors: u64, disk_guid: Guid, parts: &[Spec]) -> Result<Layout, BuildError> {
    if disk_sectors < 2 * FIRST_USABLE + 1 {
        return Err(BuildError::DiskTooSmall);
    }
    if parts.len() > ENTRIES as usize {
        return Err(BuildError::TooManyPartitions);
    }
    let last = last_usable(disk_sectors);
    let mut entries: Vec<Entry> = Vec::new();
    for (slot, spec) in parts.iter().enumerate() {
        let index = slot as u32 + 1;
        if spec.type_guid.is_zero() {
            return Err(BuildError::Type(index));
        }
        if spec.first_lba > spec.last_lba || spec.first_lba < FIRST_USABLE || spec.last_lba > last {
            return Err(BuildError::Extent(index));
        }
        let mut name = [0u16; NAME_UNITS];
        let mut units = spec.name.encode_utf16();
        for unit in name.iter_mut() {
            *unit = units.next().unwrap_or(0);
        }
        if units.next().is_some() {
            return Err(BuildError::Name(index));
        }
        let entry = Entry {
            index,
            type_guid: spec.type_guid,
            unique_guid: spec.unique_guid,
            first_lba: spec.first_lba,
            last_lba: spec.last_lba,
            attributes: 0,
            name,
        };
        entries.push(entry);
        if crate::entry::any_overlap(&entries) {
            return Err(BuildError::Overlap(index));
        }
    }
    let mut array = vec![0u8; (ARRAY_SECTORS as usize) * SECTOR];
    for (raw, entry) in array
        .as_chunks_mut::<{ ENTRY_SIZE as usize }>()
        .0
        .iter_mut()
        .zip(&entries)
    {
        raw.copy_from_slice(&entry.to_bytes());
    }
    let primary = Header {
        my_lba: 1,
        alternate_lba: disk_sectors - 1,
        first_usable: FIRST_USABLE,
        last_usable: last,
        disk_guid,
        entries_lba: 2,
        entry_count: ENTRIES,
        entry_size: ENTRY_SIZE,
        entries_crc: crc32(&array),
    };
    let backup = Header {
        my_lba: disk_sectors - 1,
        alternate_lba: 1,
        entries_lba: last + 1,
        ..primary
    };
    let mut head = vec![0u8; FIRST_USABLE as usize * SECTOR];
    head[..SECTOR].copy_from_slice(&protective_mbr(disk_sectors));
    head[SECTOR..2 * SECTOR].copy_from_slice(&primary.to_sector());
    head[2 * SECTOR..].copy_from_slice(&array);
    let mut tail = array;
    tail.extend_from_slice(&backup.to_sector());
    Ok(Layout {
        head,
        tail_lba: last + 1,
        tail,
    })
}

/// A protective MBR: one `0xEE` entry from LBA 1 over the rest of the disk
/// (capped at the 32-bit maximum, as the spec says).
pub fn protective_mbr(disk_sectors: u64) -> [u8; SECTOR] {
    let mut mbr = [0u8; SECTOR];
    let entry = &mut mbr[446..462];
    entry[1..4].copy_from_slice(&[0x00, 0x02, 0x00]);
    entry[4] = 0xEE;
    entry[5..8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
    entry[8..12].copy_from_slice(&1u32.to_le_bytes());
    let size = (disk_sectors - 1).min(u64::from(u32::MAX)) as u32;
    entry[12..16].copy_from_slice(&size.to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    mbr
}
