//! Directory iteration for the FAT volume: one bounded walker over either the
//! fixed root region or a directory's cluster chain, and a scanner on top
//! that turns raw 32-byte slots (short and long-name entries) into
//! [`Entry`] values. All on-disk bytes are untrusted (issue #235).

use super::lfn::LfnRun;
use super::{le16, le32, Fat16};
use crate::block::SECTOR_SIZE;
use crate::fs::vfs::FsError;
use alloc::string::String;

const ENTRIES_PER_SECTOR: usize = SECTOR_SIZE / 32;

/// A directory entry, after long-name assembly.
pub(super) struct Entry {
    /// The long name when the run is valid, else the short 8.3 name.
    pub name: String,
    /// The 8.3 name, kept so both spellings resolve.
    pub short: String,
    pub size: u32,
    pub cluster: u16,
    pub is_dir: bool,
    /// On-disk position of the short entry: unique across the volume.
    pub ino: u64,
}

/// Where a directory's entries live.
#[derive(Clone, Copy)]
pub(super) enum DirLoc {
    /// The fixed root region of FAT12/16.
    Root,
    /// A cluster chain starting here.
    Chain(u16),
}

type Slot = Result<(u64, [u8; 32]), FsError>;

/// Walks the 32-byte slots of one directory.
struct RawIter<'a> {
    fs: &'a Fat16,
    loc: DirLoc,
    /// Current cluster (chain directories only).
    cluster: u16,
    /// Sector index within the root region or the current cluster.
    sector: u32,
    /// Next slot within the loaded sector.
    slot: usize,
    /// Clusters entered so far, capped by the volume's cluster count.
    steps: u32,
    /// Root slots not yet yielded.
    root_left: u32,
    buf: Option<(u32, [u8; SECTOR_SIZE])>,
    done: bool,
}

impl<'a> RawIter<'a> {
    fn new(fs: &'a Fat16, loc: DirLoc) -> Self {
        RawIter {
            fs,
            loc,
            cluster: match loc {
                DirLoc::Root => 0,
                DirLoc::Chain(start) => start,
            },
            sector: 0,
            slot: 0,
            steps: 1,
            root_left: u32::from(fs.root_entries),
            buf: None,
            done: false,
        }
    }

    /// LBA of the sector to read next, advancing along the chain.
    fn next_lba(&mut self) -> Result<Option<u32>, FsError> {
        match self.loc {
            DirLoc::Root => {
                if self.root_left == 0 {
                    return Ok(None);
                }
                Ok(Some(self.fs.root_lba + self.sector))
            }
            DirLoc::Chain(_) => {
                if self.sector == u32::from(self.fs.sectors_per_cluster) {
                    let Some(next) = self.fs.next_cluster(self.cluster) else {
                        return Ok(None);
                    };
                    self.steps += 1;
                    if self.steps > self.fs.clusters {
                        return Err(FsError::Invalid); // cyclic chain
                    }
                    self.cluster = next;
                    self.sector = 0;
                }
                let base = self.fs.cluster_lba(self.cluster).ok_or(FsError::Invalid)?;
                Ok(Some(base + self.sector))
            }
        }
    }

    fn finish(&mut self, error: Option<FsError>) -> Option<Slot> {
        self.done = true;
        error.map(Err)
    }
}

impl Iterator for RawIter<'_> {
    type Item = Slot;

    fn next(&mut self) -> Option<Slot> {
        if self.done || (matches!(self.loc, DirLoc::Root) && self.root_left == 0) {
            return None;
        }
        if self.slot == 0 {
            let lba = match self.next_lba() {
                Ok(Some(lba)) => lba,
                Ok(None) => return self.finish(None),
                Err(error) => return self.finish(Some(error)),
            };
            match self.fs.read_sector(lba) {
                Some(data) => self.buf = Some((lba, data)),
                None => return self.finish(Some(FsError::Invalid)),
            }
        }
        let (lba, data) = self.buf.as_ref()?;
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&data[self.slot * 32..self.slot * 32 + 32]);
        let pos = u64::from(*lba) * ENTRIES_PER_SECTOR as u64 + self.slot as u64;
        self.slot += 1;
        if matches!(self.loc, DirLoc::Root) {
            self.root_left -= 1;
        }
        if self.slot == ENTRIES_PER_SECTOR {
            self.slot = 0;
            self.sector += 1;
        }
        Some(Ok((pos, bytes)))
    }
}

/// Iterates the logical entries of a directory (`.`/`..`, labels and deleted
/// slots hidden). After an `Err` the iterator ends.
pub(super) struct Scan<'a> {
    raw: RawIter<'a>,
    run: LfnRun,
}

impl Fat16 {
    pub(super) fn scan(&self, loc: DirLoc) -> Scan<'_> {
        Scan {
            raw: RawIter::new(self, loc),
            run: LfnRun::new(),
        }
    }
}

impl Iterator for Scan<'_> {
    type Item = Result<Entry, FsError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (pos, raw) = match self.raw.next()? {
                Ok(slot) => slot,
                Err(error) => return Some(Err(error)),
            };
            match raw[0] {
                0x00 => {
                    self.raw.done = true; // end of directory
                    return None;
                }
                0xE5 => {
                    self.run.reset();
                    continue;
                }
                _ => {}
            }
            let attr = raw[11];
            if attr == 0x0F {
                self.run.push(&raw);
                continue;
            }
            if attr & 0x08 != 0 {
                self.run.reset(); // volume label
                continue;
            }
            let long = self.run.take(&raw[0..11]);
            let short = short_name(&raw);
            if short == "." || short == ".." {
                continue;
            }
            return Some(Ok(Entry {
                name: long.unwrap_or_else(|| short.clone()),
                short,
                cluster: le16(&raw, 26),
                size: le32(&raw, 28),
                is_dir: attr & 0x10 != 0,
                ino: pos,
            }));
        }
    }
}

/// `NAME.EXT` from the 11-byte short field (`0x05` stands for a real `0xE5`).
fn short_name(raw: &[u8; 32]) -> String {
    let field = |bytes: &[u8]| -> String {
        bytes
            .iter()
            .take_while(|&&c| c != b' ')
            .map(|&c| c as char)
            .collect()
    };
    let mut base = [0u8; 8];
    base.copy_from_slice(&raw[0..8]);
    if base[0] == 0x05 {
        base[0] = 0xE5;
    }
    let (base, ext) = (field(&base), field(&raw[8..11]));
    if ext.is_empty() {
        base
    } else {
        alloc::format!("{base}.{ext}")
    }
}
