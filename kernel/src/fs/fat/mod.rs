//! A read-only FAT12/FAT16 driver over a block device.
//!
//! The bootloader builds the disk as MBR + a FAT partition holding the kernel
//! and any files added at build time. Small images come out as FAT12 and larger
//! ones as FAT16, so both are supported.
//!
//! A volume is opened against a named device and keeps that handle for its
//! lifetime (#244); the geometry parser and cluster-chain reader in
//! [`chain`] treat the on-disk bytes as untrusted (#235).

use crate::block::{BlockDevice, SECTOR_SIZE};
use alloc::vec::Vec;
use spin::Mutex;

use super::vfs::{
    DirEntry, FileKind, Filesystem, FsError, Id, Meta, SetAttr, Times, S_IFDIR, S_IFREG,
};

mod chain;
mod dir;
mod lfn;
mod resolve;

use resolve::{Node, PathCache};

/// Which FAT flavour the volume uses (determined by cluster count).
#[derive(Clone, Copy, PartialEq)]
enum FatKind {
    Fat12,
    Fat16,
}

/// A mounted FAT12/FAT16 volume.
///
/// The volume keeps the device it was opened from, so a second volume no
/// longer follows whatever device the block layer last selected as boot
/// (issue #244). Every read below goes through this handle.
pub struct Fat16 {
    device: &'static dyn BlockDevice,
    bytes_per_sector: u16,
    sectors_per_cluster: u8,
    fat_start: u32,
    root_lba: u32,
    data_lba: u32,
    root_entries: u16,
    /// Data clusters the geometry implies; the upper bound for any chain
    /// pointer and the cap on chain walks (issue #235).
    clusters: u32,
    kind: FatKind,
    /// The last FAT sector read. A chain walks its entries in cluster order,
    /// so consecutive lookups almost always land in the same sector; the
    /// volume is read-only, so the cache can never go stale.
    fat_cache: Mutex<Option<(u32, [u8; SECTOR_SIZE])>>,
    /// Paths resolved so far; see [`resolve`].
    paths: Mutex<PathCache>,
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

/// Read one 512-byte sector straight from a device (used before a volume
/// exists, while locating the partition).
fn read_device_sector(device: &'static dyn BlockDevice, lba: u32) -> Option<[u8; SECTOR_SIZE]> {
    let mut buf = [0u8; SECTOR_SIZE];
    device.read_sectors(u64::from(lba), &mut buf).ok()?;
    Some(buf)
}

impl Fat16 {
    /// Locate and parse the FAT volume on `device`.
    ///
    /// The device is named explicitly rather than taken from the block layer's
    /// active boot device, so a second volume cannot be read from the wrong
    /// disk (issue #244). A partition whose extents run past the device is
    /// skipped instead of trusted (issue #235).
    pub fn open(device: &'static dyn BlockDevice) -> Option<Fat16> {
        let mbr = read_device_sector(device, 0)?;
        for i in 0..4 {
            let base = 0x1BE + i * 16;
            let kind = mbr[base + 4];
            let lba = le32(&mbr, base + 8);
            let sectors = le32(&mbr, base + 12);
            let is_fat = matches!(kind, 0x01 | 0x04 | 0x06 | 0x0B | 0x0C);
            if !is_fat || sectors == 0 {
                continue;
            }
            // The partition must lie wholly inside the device; overflow or an
            // out-of-range end means the MBR is lying, so skip it.
            let end = match u64::from(lba).checked_add(u64::from(sectors)) {
                Some(end) => end,
                None => continue,
            };
            if end > device.sector_count() {
                continue;
            }
            if let Some(volume) = Self::parse(device, lba) {
                return Some(volume);
            }
        }
        // No usable partition: a ramdisk is typically a bare FAT image whose
        // sector 0 is the BPB itself (issue #5).
        match read_device_sector(device, 0) {
            Some(boot) if boot[510] == 0x55 && boot[511] == 0xAA => Self::parse(device, 0),
            _ => None,
        }
    }

    fn parse(device: &'static dyn BlockDevice, lba: u32) -> Option<Fat16> {
        let bpb = read_device_sector(device, lba)?;
        let bytes_per_sector = le16(&bpb, 11);
        let sectors_per_cluster = u32::from(bpb[13]);
        let reserved = u64::from(le16(&bpb, 14));
        let fats = u64::from(bpb[16]);
        let root_entries = le16(&bpb, 17);
        // FAT12/16 use 16-bit sector counts and FAT sizes; FAT32 uses 32-bit.
        let total_sectors = if le16(&bpb, 19) != 0 {
            u64::from(le16(&bpb, 19))
        } else {
            u64::from(le32(&bpb, 32))
        };
        let fat_size = if le16(&bpb, 22) != 0 {
            u64::from(le16(&bpb, 22))
        } else {
            u64::from(le32(&bpb, 36))
        };
        if bytes_per_sector != SECTOR_SIZE as u16
            || sectors_per_cluster == 0
            || sectors_per_cluster > 128
            || fats == 0
            || fat_size == 0
        {
            return None;
        }

        // All geometry in u64 with checked adds: on-disk fields are attacker
        // controlled and must never overflow `u32` (issue #235).
        let root_dir_sectors = (u64::from(root_entries) * 32).div_ceil(u64::from(bytes_per_sector));
        let fat_sectors = fats.checked_mul(fat_size)?;
        let lba = u64::from(lba);
        let overhead = reserved
            .checked_add(fat_sectors)?
            .checked_add(root_dir_sectors)?;
        if total_sectors <= overhead {
            return None; // no data cluster is possible
        }
        let root_lba = lba.checked_add(reserved)?.checked_add(fat_sectors)?;
        let data_lba = root_lba.checked_add(root_dir_sectors)?;
        let data_sectors = total_sectors - overhead;
        let clusters = data_sectors / u64::from(sectors_per_cluster);
        let kind = if clusters < 4085 {
            FatKind::Fat12
        } else if clusters < 65525 {
            FatKind::Fat16
        } else {
            return None; // FAT32 not supported
        };
        // The whole volume (data end included) must fit the device.
        let data_end =
            data_lba.checked_add(clusters.checked_mul(u64::from(sectors_per_cluster))?)?;
        if data_end > device.sector_count() {
            return None;
        }

        Some(Fat16 {
            device,
            bytes_per_sector,
            sectors_per_cluster: sectors_per_cluster as u8,
            fat_start: u32::try_from(lba.checked_add(reserved)?).ok()?,
            root_lba: u32::try_from(root_lba).ok()?,
            data_lba: u32::try_from(data_lba).ok()?,
            root_entries,
            clusters: clusters as u32,
            kind,
            fat_cache: Mutex::new(None),
            paths: Mutex::new(PathCache::default()),
        })
    }
}

/// Metadata for a resolved node. The volume has no owner, so nodes are
/// root-owned; directories and files are readable/executable by everyone
/// (`0555`, the vfat default), and nothing is writable.
fn meta_for(entry: &Node) -> Meta {
    Meta {
        ino: entry.ino,
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
        // The entry's DOS write date is not converted yet, so boot-volume
        // files report the epoch.
        times: Times::default(),
    }
}

/// The read-only FAT volume behind the [`Filesystem`] trait.
///
/// Paths resolve through nested directories and VFAT long names (issue #414,
/// see [`resolve`]). Every mutating
/// method answers [`FsError::ReadOnly`]: the friendly `EROFS` the ABI layer
/// reports when userspace tries to write.
impl Filesystem for Fat16 {
    fn name(&self) -> &'static str {
        "fat16 (ro)"
    }

    fn lookup(&self, path: &str) -> Result<Meta, FsError> {
        let entry = self.resolve(path)?;
        // A directory entry claiming more bytes than the volume can hold is
        // corrupt; refuse it before a caller sizes an allocation from it.
        if entry.size as u64 > self.capacity_bytes() {
            return Err(FsError::Invalid);
        }
        Ok(meta_for(&entry))
    }

    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let entry = self.resolve(path)?;
        if entry.is_dir {
            return Err(FsError::IsDir);
        }
        if entry.size as u64 > self.capacity_bytes() {
            return Err(FsError::Invalid);
        }
        // A non-empty file must start at a real data cluster; 0/1 and values
        // past the volume are the same corruption `next_cluster` rejects.
        if entry.size > 0 && (entry.cluster < 2 || u32::from(entry.cluster) > self.clusters + 1) {
            return Err(FsError::Invalid);
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

    fn setattr(&self, _path: &str, _attr: &SetAttr) -> Result<Meta, FsError> {
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
        let node = self.resolve(path)?;
        if !node.is_dir {
            return Err(FsError::NotDir);
        }
        Ok(self
            .list_dir(&node)?
            .into_iter()
            .map(|entry| DirEntry {
                ino: entry.ino,
                kind: if entry.is_dir {
                    FileKind::Dir
                } else {
                    FileKind::File
                },
                name: entry.name(),
            })
            .collect())
    }
}
