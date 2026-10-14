//! Finding the FAT volumes on a disk: the whole disk (a "superfloppy"), or
//! the partitions of its MBR or GPT.
//!
//! The partition table is hostile input like everything else on a stick: an
//! entry is a candidate only when its extent lies inside the disk, and a
//! candidate is only ever tried, never trusted ([`crate::bpb::validate`] and
//! the library's own checks decide whether a volume is there).

use alloc::vec::Vec;

use crate::bpb;
use crate::io::{BlockIo, Fault, SECTOR};

/// Most candidates one disk offers.
pub const MAX_CANDIDATES: usize = 16;

/// A run of sectors that may hold a volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub start: u64,
    pub sectors: u64,
}

/// MBR partition types that hold FAT (12, 16, 32 and the LBA variants, and
/// the EFI system partition).
const MBR_FAT: [u8; 7] = [0x01, 0x04, 0x06, 0x0B, 0x0C, 0x0E, 0xEF];

/// A window onto part of a device, as a device of its own.
pub struct Slice<D> {
    dev: D,
    window: Window,
}

impl<D: BlockIo> Slice<D> {
    pub fn new(dev: D, window: Window) -> Slice<D> {
        Slice { dev, window }
    }

    fn check(&self, lba: u64, bytes: usize) -> Result<u64, Fault> {
        let sectors = (bytes / SECTOR) as u64;
        let end = lba.checked_add(sectors).ok_or(Fault::Eof)?;
        if end > self.window.sectors || !bytes.is_multiple_of(SECTOR) {
            return Err(Fault::Eof);
        }
        self.window.start.checked_add(lba).ok_or(Fault::Eof)
    }
}

impl<D: BlockIo> BlockIo for Slice<D> {
    fn sectors(&self) -> u64 {
        self.window.sectors
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), Fault> {
        let at = self.check(lba, buf.len())?;
        self.dev.read(at, buf)
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), Fault> {
        let at = self.check(lba, buf.len())?;
        self.dev.write(at, buf)
    }

    fn flush(&mut self) -> Result<(), Fault> {
        self.dev.flush()
    }
}

/// [`gpt::Reader`] over a device.
struct Reader<'a, D>(&'a mut D);

impl<D: BlockIo> gpt::Reader for Reader<'_, D> {
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        let sectors = self.0.sectors();
        let want = (buf.len() / SECTOR) as u64;
        if lba.checked_add(want).is_none_or(|end| end > sectors) {
            return false;
        }
        buf.chunks_mut(crate::io::MAX_IO)
            .scan(lba, |at, chunk| {
                let here = *at;
                *at += (chunk.len() / SECTOR) as u64;
                Some((here, chunk))
            })
            .all(|(here, chunk)| self.0.read(here, chunk).is_ok())
    }
}

/// The places a volume may be on `dev`, best first: the whole device when
/// its first sector is a FAT boot sector, else the entries of its partition
/// table that fit the disk.
pub fn candidates<D: BlockIo>(dev: &mut D) -> Result<Vec<Window>, Fault> {
    let total = dev.sectors();
    let mut first = [0u8; SECTOR];
    dev.read(0, &mut first)?;
    if bpb::validate(&first, total).is_ok() {
        return Ok(alloc::vec![Window {
            start: 0,
            sectors: total,
        }]);
    }
    let mut found = Vec::new();
    if first[510..512] != [0x55, 0xAA] {
        return Ok(found);
    }
    if gpt::is_protective(&first) {
        if let Ok(table) = gpt::read(&mut Reader(dev), total) {
            for entry in &table.entries {
                let window = Window {
                    start: entry.first_lba,
                    sectors: entry.sectors(),
                };
                if fits(window, total) && found.len() < MAX_CANDIDATES {
                    found.push(window);
                }
            }
        }
        return Ok(found);
    }
    for slot in 0..4 {
        let raw = &first[446 + slot * 16..446 + slot * 16 + 16];
        if !MBR_FAT.contains(&raw[4]) {
            continue;
        }
        let start = u64::from(u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]));
        let sectors = u64::from(u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]));
        let window = Window { start, sectors };
        if fits(window, total) {
            found.push(window);
        }
    }
    Ok(found)
}

fn fits(window: Window, total: u64) -> bool {
    window.sectors > 0
        && window.start >= 1
        && window
            .start
            .checked_add(window.sectors)
            .is_some_and(|end| end <= total)
}
