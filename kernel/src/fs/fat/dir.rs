//! Directory iteration for the FAT12/16 reader (issue #414).
//!
//! A directory is either the fixed root region or a chain of clusters. Both
//! are read through one bounded iterator that yields whole entries (long name
//! attached, `.`/`..`/labels/deleted slots hidden). Every byte is untrusted:
//! a chain walk is capped by the volume's cluster count, so a directory whose
//! FAT entries loop ends with an error instead of spinning, and a bad start
//! cluster or chain pointer is an error too, never a read outside the data
//! area.

use alloc::format;
use alloc::string::String;

use super::chain::Step;
use super::lfn::{is_lfn_slot, LfnBuilder};
use super::{le16, le32, Fat16};
use crate::block::SECTOR_SIZE;
use crate::fs::vfs::FsError;

/// Directory slots (32 bytes each) per sector.
const SLOTS_PER_SECTOR: u32 = (SECTOR_SIZE / 32) as u32;
const ATTR_LABEL: u8 = 0x08;
const ATTR_DIR: u8 = 0x10;

/// Where a directory's slots live.
#[derive(Clone, Copy)]
pub(super) enum Dir {
    /// The fixed root region of a FAT12/16 volume.
    Root,
    /// A subdirectory: a cluster chain starting here.
    Chain(u16),
}

/// One directory entry: a short 8.3 slot plus its long name, if it has a
/// valid one.
pub(super) struct Entry {
    long: Option<String>,
    short: [u8; 11],
    pub size: u32,
    pub cluster: u16,
    pub is_dir: bool,
    /// Distinct per on-disk slot, never `0` or the root's `1`.
    pub ino: u64,
}

impl Entry {
    /// The name as stored: the long name when a valid one exists.
    pub fn name(&self) -> String {
        match &self.long {
            Some(long) => long.clone(),
            None => self.short_name(),
        }
    }

    /// The 8.3 name, `NAME.EXT` (or `NAME`).
    fn short_name(&self) -> String {
        let text = |bytes: &[u8]| -> String {
            bytes
                .iter()
                .take_while(|&&c| c != b' ')
                .map(|&c| c as char)
                .collect()
        };
        let (base, ext) = (text(&self.short[..8]), text(&self.short[8..]));
        // 0x05 stands in for a leading 0xE5 (which would read as "deleted").
        let base = match base.strip_prefix('\u{5}') {
            Some(rest) => format!("\u{e5}{rest}"),
            None => base,
        };
        if ext.is_empty() {
            base
        } else {
            format!("{base}.{ext}")
        }
    }
}

/// The raw slots of one directory, in order.
struct Slots<'a> {
    fat: &'a Fat16,
    dir: Dir,
    /// Root: slot index in the region. Chain: slot index in `cluster`.
    index: u32,
    cluster: u16,
    /// Chain hops taken; the cycle guard.
    hops: u32,
    /// The sector the last slot came from (16 slots share one read).
    sector: Option<(u32, [u8; SECTOR_SIZE])>,
    done: bool,
}

impl<'a> Slots<'a> {
    fn new(fat: &'a Fat16, dir: Dir) -> Result<Slots<'a>, FsError> {
        let cluster = match dir {
            Dir::Root => 0,
            Dir::Chain(start) => {
                fat.cluster_lba(start).ok_or(FsError::Invalid)?;
                start
            }
        };
        Ok(Slots {
            fat,
            dir,
            index: 0,
            cluster,
            hops: 0,
            sector: None,
            done: false,
        })
    }

    /// The next slot and its position id, `None` at the end of the directory.
    /// After an error the iterator stays finished.
    fn next(&mut self) -> Result<Option<([u8; 32], u64)>, FsError> {
        if self.done {
            return Ok(None);
        }
        let result = self.advance();
        if !matches!(result, Ok(Some(_))) {
            self.done = true;
        }
        result
    }

    fn advance(&mut self) -> Result<Option<([u8; 32], u64)>, FsError> {
        let fat = self.fat;
        let sector_lba = match self.dir {
            Dir::Root => {
                if self.index >= u32::from(fat.root_entries) {
                    return Ok(None);
                }
                fat.root_lba.checked_add(self.index / SLOTS_PER_SECTOR)
            }
            Dir::Chain(_) => {
                let per_cluster = u32::from(fat.sectors_per_cluster) * SLOTS_PER_SECTOR;
                if self.index >= per_cluster {
                    match fat.chain_step(self.cluster) {
                        Step::Next(next) => {
                            self.hops += 1;
                            if self.hops > fat.clusters {
                                return Err(FsError::Invalid); // cyclic chain
                            }
                            self.cluster = next;
                            self.index = 0;
                        }
                        Step::End => return Ok(None),
                        Step::Bad => return Err(FsError::Invalid),
                    }
                }
                fat.cluster_lba(self.cluster)
                    .and_then(|lba| lba.checked_add(self.index / SLOTS_PER_SECTOR))
            }
        }
        .ok_or(FsError::Invalid)?;

        if self
            .sector
            .as_ref()
            .is_none_or(|(lba, _)| *lba != sector_lba)
        {
            let buf = fat.read_sector(sector_lba).ok_or(FsError::Invalid)?;
            self.sector = Some((sector_lba, buf));
        }
        let (_, buf) = self.sector.as_ref().ok_or(FsError::Invalid)?;
        let slot = (self.index % SLOTS_PER_SECTOR) as usize;
        let mut raw = [0u8; 32];
        raw.copy_from_slice(&buf[slot * 32..slot * 32 + 32]);
        self.index += 1;
        Ok(Some((raw, (u64::from(sector_lba) << 4) | slot as u64)))
    }
}

/// Iterator over a directory's entries; yields one `Err` and then stops if
/// the directory is corrupt.
pub(super) struct Entries<'a> {
    slots: Slots<'a>,
    lfn: LfnBuilder,
}

impl Iterator for Entries<'_> {
    type Item = Result<Entry, FsError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (raw, position) = match self.slots.next() {
                Ok(Some(slot)) => slot,
                Ok(None) => return None,
                Err(error) => return Some(Err(error)),
            };
            match raw[0] {
                0x00 => {
                    self.slots.done = true; // nothing follows the end marker
                    return None;
                }
                0xE5 => {
                    self.lfn.reset(); // a deleted slot orphans any pending run
                    continue;
                }
                _ => {}
            }
            if is_lfn_slot(&raw) {
                self.lfn.feed(&raw);
                continue;
            }
            if raw[11] & ATTR_LABEL != 0 {
                self.lfn.reset();
                continue;
            }
            let mut short = [0u8; 11];
            short.copy_from_slice(&raw[..11]);
            let long = self.lfn.finish(&short);
            if short == *b".          " || short == *b"..         " {
                continue;
            }
            return Some(Ok(Entry {
                long,
                short,
                size: le32(&raw, 28),
                cluster: le16(&raw, 26),
                is_dir: raw[11] & ATTR_DIR != 0,
                // +1 keeps position 0 off inode 0; doubling keeps every entry
                // even, clear of the root's inode 1.
                ino: (position + 1) * 2,
            }));
        }
    }
}

impl Fat16 {
    /// Iterate the entries of `dir`.
    pub(super) fn entries(&self, dir: Dir) -> Result<Entries<'_>, FsError> {
        Ok(Entries {
            slots: Slots::new(self, dir)?,
            lfn: LfnBuilder::new(),
        })
    }

    /// Find `name` in `dir`, ASCII case-insensitively. A long name wins over
    /// another entry's 8.3 alias of the same spelling.
    pub(super) fn find_in(&self, dir: Dir, name: &str) -> Result<Option<Entry>, FsError> {
        let mut alias = None;
        for entry in self.entries(dir)? {
            let entry = entry?;
            let short_hit = entry.short_name().eq_ignore_ascii_case(name);
            match &entry.long {
                Some(long) if long.eq_ignore_ascii_case(name) => return Ok(Some(entry)),
                Some(_) if short_hit => {
                    alias.get_or_insert(entry);
                }
                None if short_hit => return Ok(Some(entry)),
                _ => {}
            }
        }
        Ok(alias)
    }
}
