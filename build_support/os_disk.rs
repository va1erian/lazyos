//! The disk side of the OS image: fixed geometry, the MBR entry for the OS
//! volume, and a file-backed [`BlockIo`] the build formats and updates it with.
//!
//! Geometry is fixed so an update never moves data. The bootloader writes MBR
//! entry 1 (stage 2) and entry 2 (the FAT `/boot` volume) from LBA 1; the OS
//! volume is entry 3 at [`OS_START_LBA`] (64 MiB), and the build fails if the
//! FAT volume would reach it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Mutex;

use ext2fs::{BlockIo, IoError};

/// Bytes per sector.
pub const SECTOR: u64 = 512;
/// Where the OS volume starts: 64 MiB, whatever the size of `/boot`.
pub const OS_START_LBA: u64 = 131_072;
/// MBR partition type of a Linux filesystem.
pub const OS_PART_TYPE: u8 = 0x83;
/// Default OS volume size (`LAZYOS_OS_SIZE`).
pub const DEFAULT_OS_SIZE: u64 = 512 << 20;
/// Smallest OS volume the build accepts.
pub const MIN_OS_SIZE: u64 = 128 << 20;

const TABLE: usize = 0x1BE;
const ENTRY: usize = 16;
const SIGNATURE: usize = 510;

/// `LAZYOS_OS_SIZE`: bytes with an optional `K`, `M` or `G` suffix (binary),
/// at least [`MIN_OS_SIZE`], rounded down to a whole 4 KiB block.
pub fn parse_size(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let (digits, shift) = match text.chars().last() {
        Some('K' | 'k') => (&text[..text.len() - 1], 10),
        Some('M' | 'm') => (&text[..text.len() - 1], 20),
        Some('G' | 'g') => (&text[..text.len() - 1], 30),
        _ => (text, 0),
    };
    let count: u64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("LAZYOS_OS_SIZE={text:?} is not a size like 512M or 2G"))?;
    let bytes = count
        .checked_mul(1 << shift)
        .filter(|bytes| *bytes <= (u64::from(u32::MAX) - OS_START_LBA) * SECTOR)
        .ok_or_else(|| format!("LAZYOS_OS_SIZE={text:?} is too large"))?
        & !4095;
    if bytes < MIN_OS_SIZE {
        return Err(format!(
            "LAZYOS_OS_SIZE={text:?} is below the {}M minimum",
            MIN_OS_SIZE >> 20
        ));
    }
    Ok(bytes)
}

/// One MBR partition entry: (type, first LBA, sector count). `n` is 1 to 4.
pub fn mbr_entry(mbr: &[u8], n: usize) -> Option<(u8, u64, u64)> {
    let at = TABLE + (n - 1) * ENTRY;
    let raw = mbr.get(at..at + ENTRY)?;
    let word = |from: usize| u64::from(u32::from_le_bytes(raw[from..from + 4].try_into().unwrap()));
    Some((raw[4], word(8), word(12)))
}

/// Whether `mbr` carries the 0x55AA boot signature.
pub fn has_signature(mbr: &[u8]) -> bool {
    mbr.get(SIGNATURE..SIGNATURE + 2) == Some(&[0x55, 0xAA])
}

/// Fail when the FAT `/boot` volume (entry 2) ends past the OS volume's start,
/// or entry 3 is already taken.
pub fn check_boot_fits(bios: &[u8]) -> Result<(), String> {
    let (kind, start, sectors) = mbr_entry(bios, 2).ok_or("the bootloader image has no MBR")?;
    if kind == 0 || !has_signature(bios) {
        return Err("the bootloader image has no FAT partition in MBR entry 2".into());
    }
    if start + sectors > OS_START_LBA {
        return Err(format!(
            "the FAT /boot partition ends at LBA {} (past the OS volume at LBA {OS_START_LBA}, \
             64 MiB); trim what goes on /boot (LAZYOS_RAMDISK?) so the OS volume keeps its place",
            start + sectors
        ));
    }
    match mbr_entry(bios, 3) {
        Some((0, _, _)) => Ok(()),
        _ => Err("MBR entry 3 is already in use".into()),
    }
}

/// Write the OS volume's entry (3) into `mbr` for a volume of `os_bytes`.
pub fn add_os_entry(mbr: &mut [u8], os_bytes: u64) {
    let at = TABLE + 2 * ENTRY;
    let entry = &mut mbr[at..at + ENTRY];
    entry.fill(0);
    entry[4] = OS_PART_TYPE;
    entry[8..12].copy_from_slice(&(OS_START_LBA as u32).to_le_bytes());
    entry[12..16].copy_from_slice(&((os_bytes / SECTOR) as u32).to_le_bytes());
}

/// The sector range of `file` starting at `start_lba`, `sectors` long, as a
/// [`BlockIo`]. Writes are refused when `writable` is false.
pub struct FileIo {
    file: Mutex<File>,
    start_lba: u64,
    sectors: u64,
    writable: bool,
}

impl FileIo {
    pub fn new(file: File, start_lba: u64, sectors: u64, writable: bool) -> FileIo {
        FileIo {
            file: Mutex::new(file),
            start_lba,
            sectors,
            writable,
        }
    }

    fn seek_to(&self, file: &mut File, lba: u64, len: usize) -> Result<(), IoError> {
        let sectors = (len as u64) / SECTOR;
        if !(len as u64).is_multiple_of(SECTOR)
            || lba.checked_add(sectors).is_none_or(|e| e > self.sectors)
        {
            return Err(IoError::Failed);
        }
        file.seek(SeekFrom::Start((self.start_lba + lba) * SECTOR))
            .map(|_| ())
            .map_err(|_| IoError::Failed)
    }
}

impl BlockIo for FileIo {
    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), IoError> {
        let mut file = self.file.lock().map_err(|_| IoError::Failed)?;
        self.seek_to(&mut file, lba, buf.len())?;
        file.read_exact(buf).map_err(|_| IoError::Failed)
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), IoError> {
        if !self.writable {
            return Err(IoError::ReadOnly);
        }
        let mut file = self.file.lock().map_err(|_| IoError::Failed)?;
        self.seek_to(&mut file, lba, buf.len())?;
        file.write_all(buf).map_err(|_| IoError::Failed)
    }

    fn flush(&self) -> Result<(), IoError> {
        if !self.writable {
            return Ok(());
        }
        let mut file = self.file.lock().map_err(|_| IoError::Failed)?;
        file.flush().map_err(|_| IoError::Failed)?;
        file.sync_data().map_err(|_| IoError::Failed)
    }

    fn is_writable(&self) -> bool {
        self.writable
    }
}
