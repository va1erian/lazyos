//! A read-only FAT16 driver over the ATA block device.
//!
//! The bootloader builds the disk image as MBR + a FAT16 partition holding the
//! kernel (and any files added at build time).

use crate::block::ata;
use alloc::string::String;
use alloc::vec::Vec;

/// A directory entry.
pub struct Entry {
    pub name: String,
    pub size: u32,
    pub cluster: u16,
    pub is_dir: bool,
}

/// A mounted FAT16 volume.
pub struct Fat16 {
    partition_lba: u32,
    bytes_per_sector: u16,
    sectors_per_cluster: u8,
    reserved: u16,
    root_entries: u16,
    root_lba: u32,
    data_lba: u32,
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
    /// Locate and parse the FAT16 volume on the primary disk.
    pub fn open() -> Option<Fat16> {
        let mbr = read_sector(0)?;
        // Scan the four MBR partition entries.
        for i in 0..4 {
            let base = 0x1BE + i * 16;
            let kind = mbr[base + 4];
            let lba = le32(&mbr, base + 8);
            let sectors = le32(&mbr, base + 12);
            let fat_type = matches!(kind, 0x01 | 0x04 | 0x06 | 0x0B | 0x0C);
            if !fat_type || sectors == 0 {
                continue;
            }
            let bpb = read_sector(lba)?;
            let bytes_per_sector = le16(&bpb, 11);
            let fat_size = le16(&bpb, 22);
            let root_entries = le16(&bpb, 17);
            // FAT12/16 have non-zero 16-bit fields; FAT32 leaves them zero.
            if bytes_per_sector != 512 || fat_size == 0 || root_entries == 0 {
                continue;
            }
            let reserved = le16(&bpb, 14);
            let fats = bpb[16];
            let sectors_per_cluster = bpb[13];
            let root_lba = lba + reserved as u32 + fats as u32 * fat_size as u32;
            let data_lba = root_lba + (root_entries as u32 * 32).div_ceil(512);
            return Some(Fat16 {
                partition_lba: lba,
                bytes_per_sector,
                sectors_per_cluster,
                reserved,
                root_entries,
                root_lba,
                data_lba,
            });
        }
        None
    }

    fn fat_start(&self) -> u32 {
        self.partition_lba + self.reserved as u32
    }

    fn cluster_lba(&self, cluster: u16) -> u32 {
        self.data_lba + (cluster as u32 - 2) * self.sectors_per_cluster as u32
    }

    /// Next cluster in the chain (FAT16 entries are 16-bit).
    fn next_cluster(&self, cluster: u16) -> Option<u16> {
        let fat_offset = cluster as u32 * 2;
        let sector = self.fat_start() + fat_offset / self.bytes_per_sector as u32;
        let index = (fat_offset % self.bytes_per_sector as u32) as usize;
        let buf = read_sector(sector)?;
        let next = le16(&buf, index);
        if next >= 0xFFF8 {
            None // end of chain
        } else if next == 0 {
            None
        } else {
            Some(next)
        }
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

            let first = entry[0];
            if first == 0x00 {
                break; // end of directory
            }
            if first == 0xE5 {
                continue; // deleted
            }
            let attr = entry[11];
            if attr == 0x0F || attr & 0x08 != 0 {
                continue; // LFN or volume label
            }

            let name = format_name(&entry[0..8], &entry[8..11]);
            let cluster = le16(entry, 26);
            let size = le32(entry, 28);
            entries.push(Entry {
                name,
                size,
                cluster,
                is_dir: attr & 0x10 != 0,
            });
        }
        entries
    }

    /// Read a file by name (case-insensitive, `NAME.EXT` or `NAME`).
    pub fn read(&self, name: &str) -> Option<Vec<u8>> {
        let wanted = normalize(name);
        for entry in self.list() {
            if entry.is_dir {
                continue;
            }
            let candidate = normalize(&entry.name);
            let matches = match (&wanted, &candidate) {
                (Some(w), Some(c)) => w == c,
                _ => entry.name.eq_ignore_ascii_case(name),
            };
            if matches {
                return self.read_clusters(entry.cluster, entry.size);
            }
        }
        None
    }

    fn read_clusters(&self, start: u16, size: u32) -> Option<Vec<u8>> {
        let mut data = Vec::with_capacity(size as usize);
        if start < 2 {
            return Some(data);
        }
        let mut cluster = start;
        // Bounded to avoid an infinite loop on a corrupt chain.
        for _ in 0..0x10000 {
            let lba = self.cluster_lba(cluster);
            for s in 0..self.sectors_per_cluster as u32 {
                let buf = read_sector(lba + s)?;
                data.extend_from_slice(&buf);
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
        Some((b, e)) => (b, e),
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
