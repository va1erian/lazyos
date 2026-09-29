//! A read-only FAT12/FAT16 driver over the ATA block device.
//!
//! The bootloader builds the disk as MBR + a FAT partition holding the kernel
//! and any files added at build time. Small images come out as FAT12 and larger
//! ones as FAT16, so both are supported.

use crate::block::{self, ata};
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use super::vfs::{DirEntry, FileKind, Filesystem, FsError, Id, Meta, S_IFDIR, S_IFREG};

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
    /// The last FAT sector read. A chain walks its entries in cluster order,
    /// so consecutive lookups almost always land in the same sector; the
    /// volume is read-only, so the cache can never go stale.
    fat_cache: Mutex<Option<(u32, [u8; 512])>>,
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
            fat_cache: Mutex::new(None),
        })
    }

    fn cluster_lba(&self, cluster: u16) -> u32 {
        self.data_lba + (cluster as u32 - 2) * self.sectors_per_cluster as u32
    }

    /// Read a little-endian 16-bit FAT entry that may straddle a sector edge.
    fn read_fat_word(&self, sector: u32, index: usize) -> Option<u16> {
        let buf = self.fat_sector(sector)?;
        let low = buf[index] as u16;
        let high = if index + 1 < self.bytes_per_sector as usize {
            buf[index + 1] as u16
        } else {
            self.fat_sector(sector + 1)?[0] as u16
        };
        Some(low | (high << 8))
    }

    /// A FAT sector through the one-entry cache.
    fn fat_sector(&self, lba: u32) -> Option<[u8; 512]> {
        let mut cache = self.fat_cache.lock();
        if let Some((cached, data)) = cache.as_ref() {
            if *cached == lba {
                return Some(*data);
            }
        }
        let data = read_sector(lba)?;
        *cache = Some((lba, data));
        Some(data)
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
                if cluster.is_multiple_of(2) {
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
    pub fn list(&self) -> Vec<Entry> {
        let mut entries = Vec::new();
        let mut offset = 0u32;
        // One sector holds 16 entries: read it once, not once per entry.
        let mut loaded: Option<(u32, [u8; 512])> = None;
        while offset < self.root_entries as u32 {
            let sector = self.root_lba + offset / 16;
            if loaded.as_ref().is_none_or(|(lba, _)| *lba != sector) {
                let Some(buf) = read_sector(sector) else {
                    break;
                };
                loaded = Some((sector, buf));
            }
            let Some((_, buf)) = loaded.as_ref() else {
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

    /// Read `buf.len()` bytes at `offset` without loading the whole chain: walk
    /// to the first cluster, then read only the sectors the range touches.
    fn read_at(&self, start: u16, size: u32, offset: u64, buf: &mut [u8]) -> Option<usize> {
        if offset >= size as u64 || start < 2 {
            return Some(0);
        }
        let cluster_bytes = self.sectors_per_cluster as u64 * self.bytes_per_sector as u64;
        let mut cluster = start;
        let mut skip = offset / cluster_bytes;
        while skip > 0 {
            cluster = self.next_cluster(cluster)?;
            skip -= 1;
        }

        let mut inner = (offset % cluster_bytes) as usize;
        let remaining = (size as u64 - offset).min(buf.len() as u64) as usize;
        let mut written = 0usize;
        while written < remaining {
            // Extend the run over clusters that follow on disk (the image
            // builder lays files out contiguously), so one device command
            // covers many clusters instead of one per cluster.
            let want = remaining - written;
            let mut span = cluster_bytes as usize - inner;
            let mut last = cluster;
            let mut following = None;
            while span < want {
                match self.next_cluster(last) {
                    Some(next) if last.checked_add(1) == Some(next) => {
                        last = next;
                        span += cluster_bytes as usize;
                    }
                    other => {
                        following = other;
                        break;
                    }
                }
            }
            let take = span.min(want);
            self.read_span(
                self.cluster_lba(cluster),
                inner,
                &mut buf[written..written + take],
            )?;
            written += take;
            if written < remaining {
                cluster = following?;
                inner = 0;
            }
        }
        Some(written)
    }

    /// Fill `out` from the byte range starting `byte` bytes into the run of
    /// sectors that begins at `lba`. Whole sectors go straight into `out` in
    /// one command; only a partial head or tail sector goes through a bounce.
    fn read_span(&self, lba: u32, byte: usize, out: &mut [u8]) -> Option<()> {
        let sector_bytes = self.bytes_per_sector as usize;
        let mut next = lba + (byte / sector_bytes) as u32;
        let head = byte % sector_bytes;
        let mut rest = out;
        if head != 0 || rest.len() < sector_bytes {
            let sector = read_sector(next)?;
            let take = (sector_bytes - head).min(rest.len());
            let (now, later) = rest.split_at_mut(take);
            now.copy_from_slice(&sector[head..head + take]);
            rest = later;
            next += 1;
        }
        let whole = rest.len() / sector_bytes * sector_bytes;
        if whole > 0 {
            let (now, later) = rest.split_at_mut(whole);
            if !block::read_sectors(next, now) {
                return None;
            }
            rest = later;
            next += (whole / sector_bytes) as u32;
        }
        if !rest.is_empty() {
            let sector = read_sector(next)?;
            let tail = rest.len();
            rest.copy_from_slice(&sector[..tail]);
        }
        Some(())
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

/// Stable pseudo-inode for a FAT name (FAT has no inode numbers). Bit 1 is
/// forced, so no entry can collide with the root's inode 1 or with 0.
fn ino_for(name: &str) -> u64 {
    name.to_ascii_uppercase()
        .bytes()
        .fold(0u64, |acc, byte| acc.wrapping_mul(31) + byte as u64)
        | 2
}

/// Metadata for a FAT entry. The volume has no owner, so nodes are
/// root-owned; directories and files are readable/executable by everyone
/// (`0555`, the vfat default), and nothing is writable.
fn meta_for(entry: &Entry) -> Meta {
    Meta {
        ino: ino_for(&entry.name),
        mode: if entry.is_dir {
            S_IFDIR | 0o555
        } else {
            S_IFREG | 0o555
        },
        uid: 0,
        gid: 0,
        size: entry.size as u64,
        kind: if entry.is_dir {
            FileKind::Dir
        } else {
            FileKind::File
        },
    }
}

/// The read-only FAT volume behind the [`Filesystem`] trait.
///
/// This reader only resolves the root directory, so the mount point root is
/// the whole volume and any nested path is simply not found. Every mutating
/// method answers [`FsError::ReadOnly`]: the friendly `EROFS` the ABI layer
/// reports when userspace tries to write.
impl Filesystem for Fat16 {
    fn name(&self) -> &'static str {
        "fat16 (ro)"
    }

    fn lookup(&self, path: &str) -> Result<Meta, FsError> {
        let name = path.trim_matches('/');
        if name.is_empty() {
            return Ok(Meta {
                ino: 1,
                mode: S_IFDIR | 0o555,
                uid: 0,
                gid: 0,
                size: self.root_entries as u64 * 32,
                kind: FileKind::Dir,
            });
        }
        self.find(name)
            .map(|entry| meta_for(&entry))
            .ok_or(FsError::NotFound)
    }

    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let entry = self.find(path.trim_matches('/')).ok_or(FsError::NotFound)?;
        if entry.is_dir {
            return Err(FsError::IsDir);
        }
        self.read_at(entry.cluster, entry.size, offset, buf)
            .ok_or(FsError::Invalid)
    }

    fn write(&self, _path: &str, _offset: u64, _data: &[u8]) -> Result<usize, FsError> {
        Err(FsError::ReadOnly)
    }

    fn truncate(&self, _path: &str, _size: u64) -> Result<(), FsError> {
        Err(FsError::ReadOnly)
    }

    fn create(&self, _path: &str, _mode: u16, _owner: Id) -> Result<Meta, FsError> {
        Err(FsError::ReadOnly)
    }

    fn mkdir(&self, _path: &str, _mode: u16, _owner: Id) -> Result<Meta, FsError> {
        Err(FsError::ReadOnly)
    }

    fn unlink(&self, _path: &str) -> Result<(), FsError> {
        Err(FsError::ReadOnly)
    }

    fn rmdir(&self, _path: &str) -> Result<(), FsError> {
        Err(FsError::ReadOnly)
    }

    fn rename(&self, _from: &str, _to: &str) -> Result<(), FsError> {
        Err(FsError::ReadOnly)
    }

    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        if !path.trim_matches('/').is_empty() {
            return Err(FsError::NotDir); // root-only reader: no subdirectories
        }
        Ok(self
            .list()
            .into_iter()
            .map(|entry| DirEntry {
                ino: ino_for(&entry.name),
                kind: if entry.is_dir {
                    FileKind::Dir
                } else {
                    FileKind::File
                },
                name: entry.name,
            })
            .collect())
    }
}
