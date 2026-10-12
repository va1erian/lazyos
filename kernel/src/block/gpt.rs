//! GPT partitions (docs/grub-install-plan.md K2) over `libs/gpt`.
//!
//! A disk whose sector 0 is a protective (or hybrid) MBR is read as GPT: the
//! primary table, else the backup in the last sector, both CRC- and
//! bounds-checked by the library, which also drops entries that leave the
//! usable area or overlap. The entries of the types LazyOS can mount become
//! [`Entry`]s for [`super::partition`] to register as `<disk>p<n>`, `n` the
//! 1-based GPT index (Linux's `sdaN` numbering). When neither copy reads, the
//! caller falls back to the MBR entries, as Linux does with a hybrid MBR.

use alloc::vec::Vec;

use gpt::guid::{self, Guid};

use super::partition::{Entry, KIND_ESP, KIND_FAT32_LBA, KIND_LINUX};
use super::{BlockDevice, SECTOR_SIZE};

struct DiskReader(&'static dyn BlockDevice);

impl gpt::Reader for DiskReader {
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        self.0.read_sectors(lba, buf).is_ok()
    }
}

/// The MBR-style kind a GPT type is registered as, or `None` for a type
/// LazyOS never mounts (swap, BIOS boot, LVM, RAID, Windows reserved, ...).
pub fn kind_of(type_guid: Guid) -> Option<u8> {
    match type_guid {
        guid::ESP => Some(KIND_ESP),
        guid::BASIC_DATA => Some(KIND_FAT32_LBA),
        guid::LINUX_DATA | guid::LINUX_ROOT_X86_64 | guid::LINUX_HOME => Some(KIND_LINUX),
        _ => None,
    }
}

/// The partitions to register when `mbr` (sector 0 of `disk`) is protective;
/// `None` when it is not, or when neither copy of the table reads (the
/// caller then uses the MBR).
pub fn entries(disk: &'static dyn BlockDevice, mbr: &[u8; SECTOR_SIZE]) -> Option<Vec<Entry>> {
    if !gpt::is_protective(mbr) {
        return None;
    }
    let name = disk.name();
    let table = match gpt::read(&mut DiskReader(disk), disk.sector_count()) {
        Ok(table) => table,
        Err(failure) => {
            serial_println!(
                "block: {name}: no usable GPT (primary {:?}, backup {:?})",
                failure.primary,
                failure.backup
            );
            return None;
        }
    };
    if let Some(error) = table.primary_error {
        serial_println!("block: {name}: primary GPT unusable ({error:?}), using the backup");
    }
    for (index, error) in &table.rejected {
        serial_println!("block: {name}p{index}: GPT entry {error:?}, skipped");
    }
    let found = table
        .entries
        .iter()
        .filter_map(|entry| {
            Some(Entry {
                index: entry.index,
                kind: kind_of(entry.type_guid)?,
                lba: entry.first_lba,
                sectors: entry.sectors(),
            })
        })
        .collect();
    Some(found)
}
