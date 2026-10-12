//! Fuzz entry point, shared by the seeded tests and the cargo-fuzz target
//! (`fuzz/fuzz_targets/gpt.rs`), so a crash found by one replays under the
//! other.
//!
//! [`run`] reads a mode byte, then:
//!
//! * `0`: the rest is a whole disk (zero padded to at least 80 sectors);
//! * otherwise: the rest scripts a valid table (up to 8 partitions) that is
//!   then damaged at fuzzed offsets.
//!
//! Nothing may panic, and whatever [`crate::read`] accepts must keep its
//! promises: every entry inside the usable area and the disk, no two
//! entries overlapping, the array inside the disk.

use std::vec::Vec;

use crate::guid::{Guid, LINUX_DATA};
use crate::write::{self, Spec, FIRST_USABLE};
use crate::{Reader, SECTOR};

/// A disk held in memory.
pub struct MemDisk<'a>(pub &'a [u8]);

impl Reader for MemDisk<'_> {
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        let Some(start) = usize::try_from(lba)
            .ok()
            .and_then(|lba| lba.checked_mul(SECTOR))
        else {
            return false;
        };
        match self.0.get(start..start.saturating_add(buf.len())) {
            Some(bytes) if bytes.len() == buf.len() => {
                buf.copy_from_slice(bytes);
                true
            }
            _ => false,
        }
    }
}

pub fn run(data: &[u8]) {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    match mode {
        0 => raw_disk(rest),
        _ => damaged_table(rest),
    }
}

fn raw_disk(rest: &[u8]) {
    let mut disk = rest.to_vec();
    let len = disk.len().max(80 * SECTOR).next_multiple_of(SECTOR);
    disk.resize(len, 0);
    check(&disk);
}

fn damaged_table(rest: &[u8]) {
    let mut bytes = rest.iter().copied();
    let mut next = || bytes.next().unwrap_or(0);
    let disk_sectors = 2 * FIRST_USABLE + 1 + u64::from(next()) * 8;
    let last = write::last_usable(disk_sectors);
    let count = next() % 9;
    let mut specs = Vec::new();
    let mut at = FIRST_USABLE;
    for _ in 0..count {
        let start = at + u64::from(next() % 8);
        let end = start + u64::from(next() % 16);
        if end > last {
            break;
        }
        specs.push(Spec {
            type_guid: LINUX_DATA,
            unique_guid: Guid([next(); 16]),
            first_lba: start,
            last_lba: end,
            name: "fuzz",
        });
        at = end + 1;
    }
    let layout = write::build(disk_sectors, Guid([7; 16]), &specs).expect("a valid script");
    let mut disk = std::vec![0u8; disk_sectors as usize * SECTOR];
    layout.apply(&mut disk);
    let pristine = crate::read(&mut MemDisk(&disk), disk_sectors).expect("the written table reads");
    assert_eq!(pristine.entries.len(), specs.len());
    // Damage: pairs of (offset in the head or tail, byte).
    let damages = next() % 16;
    for _ in 0..damages {
        let tail = next() & 1 == 1;
        let offset = usize::from(next()) << 8 | usize::from(next());
        let value = next();
        let base = if tail {
            layout.tail_lba as usize * SECTOR
        } else {
            0
        };
        let span = if tail {
            layout.tail.len()
        } else {
            layout.head.len()
        };
        disk[base + offset % span] = value;
    }
    check(&disk);
}

fn check(disk: &[u8]) {
    let sectors = (disk.len() / SECTOR) as u64;
    let Ok(table) = crate::read(&mut MemDisk(disk), sectors) else {
        return;
    };
    let h = &table.header;
    assert!(h.first_usable <= h.last_usable && h.last_usable < sectors);
    assert!(h.entries_lba + h.array_sectors() <= sectors);
    for entry in &table.entries {
        assert!(entry.first_lba >= h.first_usable && entry.last_lba <= h.last_usable);
        assert!(entry.first_lba <= entry.last_lba);
        assert!(!entry.type_guid.is_zero());
        let _ = entry.name_chars().count();
    }
    assert!(!crate::entry::any_overlap(&table.entries));
}
