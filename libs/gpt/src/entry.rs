//! Partition entries: 128 bytes (or the header's larger size) each, an
//! all-zero type GUID marking an unused slot.

use alloc::vec::Vec;

use crate::guid::Guid;
use crate::header::Header;

/// UTF-16 code units in a partition name.
pub const NAME_UNITS: usize = 36;

/// Why a used entry was dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryError {
    /// `first_lba > last_lba`.
    Inverted,
    /// The extent leaves the header's usable area.
    OutsideUsable,
    /// It overlaps another used entry (both are dropped).
    Overlap,
}

/// A used, validated entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// 1-based position in the array (the `N` of `sdaN`).
    pub index: u32,
    pub type_guid: Guid,
    pub unique_guid: Guid,
    pub first_lba: u64,
    /// Inclusive, as on disk.
    pub last_lba: u64,
    pub attributes: u64,
    /// UTF-16LE, nul padded.
    pub name: [u16; NAME_UNITS],
}

impl Entry {
    /// The partition's length in sectors.
    pub fn sectors(&self) -> u64 {
        self.last_lba - self.first_lba + 1
    }

    /// The name, up to the first nul; unpaired surrogates become U+FFFD.
    pub fn name_chars(&self) -> impl Iterator<Item = char> + '_ {
        let len = self
            .name
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(NAME_UNITS);
        char::decode_utf16(self.name[..len].iter().copied())
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
    }

    /// Whether the name is exactly `name`.
    pub fn name_is(&self, name: &str) -> bool {
        self.name_chars().eq(name.chars())
    }

    fn overlaps(&self, other: &Entry) -> bool {
        self.first_lba <= other.last_lba && other.first_lba <= self.last_lba
    }

    /// Parse slot `index` (1-based) from its raw bytes; `None` when unused.
    fn parse(index: u32, raw: &[u8]) -> Option<Entry> {
        let guid = |at: usize| {
            let mut bytes = [0u8; 16];
            bytes.copy_from_slice(&raw[at..at + 16]);
            Guid(bytes)
        };
        let u64_at = |at: usize| {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&raw[at..at + 8]);
            u64::from_le_bytes(bytes)
        };
        let type_guid = guid(0);
        if type_guid.is_zero() {
            return None;
        }
        let mut name = [0u16; NAME_UNITS];
        for (unit, pair) in name.iter_mut().zip(raw[56..128].as_chunks::<2>().0) {
            *unit = u16::from_le_bytes([pair[0], pair[1]]);
        }
        Some(Entry {
            index,
            type_guid,
            unique_guid: guid(16),
            first_lba: u64_at(32),
            last_lba: u64_at(40),
            attributes: u64_at(48),
            name,
        })
    }

    /// The entry's 128 bytes (the writer's inverse of parsing).
    pub fn to_bytes(&self) -> [u8; 128] {
        let mut raw = [0u8; 128];
        raw[..16].copy_from_slice(&self.type_guid.0);
        raw[16..32].copy_from_slice(&self.unique_guid.0);
        raw[32..40].copy_from_slice(&self.first_lba.to_le_bytes());
        raw[40..48].copy_from_slice(&self.last_lba.to_le_bytes());
        raw[48..56].copy_from_slice(&self.attributes.to_le_bytes());
        for (pair, unit) in raw[56..].as_chunks_mut::<2>().0.iter_mut().zip(self.name) {
            pair.copy_from_slice(&unit.to_le_bytes());
        }
        raw
    }
}

/// Split the used entries of `array` (whose CRC already matched) into the
/// ones to use and the ones dropped, each list in index order.
pub fn validate(header: &Header, array: &[u8]) -> (Vec<Entry>, Vec<(u32, EntryError)>) {
    let size = header.entry_size as usize;
    let mut candidates = Vec::new();
    let mut rejected = Vec::new();
    for (slot, raw) in array[..header.array_bytes()].chunks_exact(size).enumerate() {
        let index = slot as u32 + 1;
        let Some(entry) = Entry::parse(index, raw) else {
            continue;
        };
        if entry.first_lba > entry.last_lba {
            rejected.push((index, EntryError::Inverted));
        } else if entry.first_lba < header.first_usable || entry.last_lba > header.last_usable {
            rejected.push((index, EntryError::OutsideUsable));
        } else {
            candidates.push(entry);
        }
    }
    // Neither of an overlapping pair can be trusted. Pairwise: a real table
    // uses a handful of slots, and the header caps the array at 1024.
    let bad: Vec<bool> = candidates
        .iter()
        .enumerate()
        .map(|(i, a)| {
            candidates
                .iter()
                .enumerate()
                .any(|(j, b)| i != j && a.overlaps(b))
        })
        .collect();
    let mut entries = Vec::new();
    for (entry, bad) in candidates.into_iter().zip(bad) {
        if bad {
            rejected.push((entry.index, EntryError::Overlap));
        } else {
            entries.push(entry);
        }
    }
    rejected.sort_by_key(|&(index, _)| index);
    (entries, rejected)
}

/// Whether any two of `entries` overlap (the invariant [`validate`] keeps).
pub fn any_overlap(entries: &[Entry]) -> bool {
    entries
        .iter()
        .enumerate()
        .any(|(i, a)| entries[i + 1..].iter().any(|b| a.overlaps(b)))
}
