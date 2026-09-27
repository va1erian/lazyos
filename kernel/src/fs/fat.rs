//! A read-only FAT12/FAT16 driver over the ATA block device.
//!
//! The bootloader builds the disk as MBR + a FAT partition holding the kernel
//! and any files added at build time. Small images come out as FAT12 and larger
//! ones as FAT16, so both are supported.

use crate::block::ata;
use alloc::string::String;
use alloc::vec::Vec;

/// Which FAT flavour the volume uses (determined by cluster count).
#[derive(Clone, Copy, PartialEq)]
enum FatKind {
    Fat12,
    Fat16,
}

/// A directory entry.
pub struct Entry {
    pub name: String,
    pub size: u32,
    pub cluster: u16,
    pub is_dir: bool,
}

/// A mounted FAT12/FAT16 volume.
pub struct Fat16 {
    bytes_per_sector: u16,
    sectors_per_cluster: u8,
    fat_start: u32,
    root_lba: u32,
    data_lba: u32,
    root_entries: u16,
    kind: FatKind,
}

fn read_sector(lba: u32) -> Option<[u8; 512]> {
    let mut buf = [0u8; 512];
    if ata::read_sector(lba, &mut buf) {
        Some(buf)
    } else {
        None
    }
}

fn le16(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

fn le32(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

impl Fat16 {
    /// Locate and parse the FAT volume on the primary disk.
    pub fn open() -> Option<Fat16> {
        let mbr = read_sector(0)?;
        for i in 0..4 {
            let base = 0x1BE + i * 16;
            let kind = mbr[base + 4];
            let lba = le32(&mbr, base + 8);
            let sectors = le32(&mbr, base + 12);
            let is_fat = matches!(kind, 0x01 | 0x04 | 0x06 | 0x0B | 0x0C);
            if !is_fat || sectors == 0 {
                continue;
            }
            if let Some(volume) = Self::parse(lba) {
                return Some(volume);
            }
        }
        None
    }

    fn parse(lba: u32) -> Option<Fat16> {
        let bpb = read_sector(lba)?;
        let bytes_per_sector = le16(&bpb, 11);
        let sectors_per_cluster = bpb[13];
        let reserved = le16(&bpb, 14) as u32;
        let fats = bpb[16] as u32;
        let root_entries = le16(&bpb, 17);
        // FAT12/16 use 16-bit sector counts and FAT sizes; FAT32 uses 32-bit.
        let total_sectors = if le16(&bpb, 19) != 0 {
            le16(&bpb, 19) as u32
        } else {
            le32(&bpb, 32)
        };
        let fat_size = if le16(&bpb, 22) != 0 {
            le16(&bpb, 22) as u32
        } else {
            le32(&bpb, 36)
        };
        if bytes_per_sector != 512 || sectors_per_cluster == 0 || fat_size == 0 {
            return None;
        }

        let root_dir_sectors = (root_entries as u32 * 32).div_ceil(bytes_per_sector as u32);
        let root_lba = lba + reserved + fats * fat_size;
        let data_lba = root_lba + root_dir_sectors;
        let data_sectors =
            total_sectors.saturating_sub(reserved + fats * fat_size + root_dir_sectors);
        let clusters = data_sectors / sectors_per_cluster as u32;
        let kind = if clusters < 4085 {
            FatKind::Fat12
        } else if clusters < 65525 {
            FatKind::Fat16
        } else {
            return None; // FAT32 not supported
        };

        Some(Fat16 {
            bytes_per_sector,
            sectors_per_cluster,
            fat_start: lba + reserved,
            root_lba,
            data_lba,
            root_entries,
            kind,
        })
    }

    fn cluster_lba(&self, cluster: u16) -> u32 {
        self.data_lba + (cluster as u32 - 2) * self.sectors_per_cluster as u32
    }

    /// Read a little-endian 16-bit FAT entry that may straddle a sector edge.
    fn read_fat_word(&self, sector: u32, index: usize) -> Option<u16> {
        let buf = read_sector(sector)?;
        let low = buf[index] as u16;
        let high = if index + 1 < self.bytes_per_sector as usize {
            buf[index + 1] as u16
        } else {
            read_sector(sector + 1)?[0] as u16
        };
        Some(low | (high << 8))
    }

    /// Next cluster in a chain, following the FAT (12- or 16-bit entries).
    fn next_cluster(&self, cluster: u16) -> Option<u16> {
        let byte_offset = match self.kind {
            FatKind::Fat12 => cluster as u32 + cluster as u32 / 2,
            FatKind::Fat16 => cluster as u32 * 2,
        };
        let sector = self.fat_start + byte_offset / self.bytes_per_sector as u32;
        let index = (byte_offset % self.bytes_per_sector as u32) as usize;

        let value = match self.kind {
            FatKind::Fat12 => {
                let word = self.read_fat_word(sector, index)?;
                // 12-bit entries are packed; pick the low or high nibble pair.
                if cluster % 2 == 0 {
                    word & 0x0FFF
                } else {
                    word >> 4
                }
            }
            FatKind::Fat16 => self.read_fat_word(sector, index)?,
        };

        let end_of_chain = match self.kind {
            FatKind::Fat12 => value >= 0xFF8,
            FatKind::Fat16 => value >= 0xFFF8,
        };
        if end_of_chain || value == 0 {
            None
        } else {
            Some(value)
        }
    }

    /// List the root directory (short 8.3 entries; long-name entries skipped).
    pub fn entries(&self) -> Vec<Entry> {
        self.list()
    }

    /// List the root directory (short 8.3 entries; long-name entries skipped).
    pub fn list(&self) -> Vec<Entry> {
        let mut entries = Vec::new();
        let mut offset = 0u32;
        while offset < self.root_entries as u32 {
            let sector = self.root_lba + offset / 16;
            let Some(buf) = read_sector(sector) else {
                break;
            };
            let index = (offset % 16) as usize * 32;
            let entry = &buf[index..index + 32];
            offset += 1;

            if entry[0] == 0x00 {
                break; // end of directory
            }
            if entry[0] == 0xE5 {
                continue; // deleted
            }
            let attr = entry[11];
            if attr == 0x0F || attr & 0x08 != 0 {
                continue; // long-name entry or volume label
            }

            entries.push(Entry {
                name: format_name(&entry[0..8], &entry[8..11]),
                cluster: le16(entry, 26),
                size: le32(entry, 28),
                is_dir: attr & 0x10 != 0,
            });
        }
        entries
    }

    /// Find a root-directory entry by name (case-insensitive).
    pub fn find(&self, name: &str) -> Option<Entry> {
        let wanted = normalize(name);
        self.list()
            .into_iter()
            .find(|entry| match (&wanted, normalize(&entry.name)) {
                (Some(want), Some(candidate)) => *want == candidate,
                _ => entry.name.eq_ignore_ascii_case(name),
            })
    }

    /// Metadata for an entry: `(size, is_dir)`.
    pub fn stat(&self, name: &str) -> Option<(u32, bool)> {
        self.find(name).map(|entry| (entry.size, entry.is_dir))
    }

    /// Read a file by name (case-insensitive, `NAME.EXT` or `NAME`).
    pub fn read(&self, name: &str) -> Option<Vec<u8>> {
        let entry = self.find(name)?;
        if entry.is_dir {
            return None;
        }
        self.read_clusters(entry.cluster, entry.size)
    }

    fn read_clusters(&self, start: u16, size: u32) -> Option<Vec<u8>> {
        let mut data = Vec::with_capacity(size as usize);
        if start < 2 {
            return Some(data);
        }
        let mut cluster = start;
        // Bounded so a corrupt chain cannot loop forever.
        for _ in 0..0x10000 {
            let lba = self.cluster_lba(cluster);
            for sector in 0..self.sectors_per_cluster as u32 {
                data.extend_from_slice(&read_sector(lba + sector)?);
            }
            if data.len() as u32 >= size {
                data.truncate(size as usize);
                break;
            }
            match self.next_cluster(cluster) {
                Some(next) => cluster = next,
                None => break,
            }
        }
        Some(data)
    }
}

fn format_name(base: &[u8], ext: &[u8]) -> String {
    let base: String = base
        .iter()
        .take_while(|&&c| c != b' ')
        .map(|&c| c as char)
        .collect();
    let ext: String = ext
        .iter()
        .take_while(|&&c| c != b' ')
        .map(|&c| c as char)
        .collect();
    if ext.is_empty() {
        base
    } else {
        alloc::format!("{base}.{ext}")
    }
}

/// Normalize `NAME.EXT` to an uppercase "NAME.EXT" or "NAME" key.
fn normalize(name: &str) -> Option<String> {
    let (base, ext) = match name.split_once('.') {
        Some((base, ext)) => (base, ext),
        None => (name, ""),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 {
        return None;
    }
    let mut key = base.to_ascii_uppercase();
    if !ext.is_empty() {
        key.push('.');
        key.push_str(&ext.to_ascii_uppercase());
    }
    Some(key)
}
