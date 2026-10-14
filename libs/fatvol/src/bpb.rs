//! A strict check of a FAT boot sector, before `rust-fatfs` sees it.
//!
//! The library validates the fields it uses, but not all of them in a way
//! that stops its arithmetic: a damaged sectors-per-FAT or root-cluster
//! field made it overflow a `u32` (a panic with overflow checks, a wild
//! offset without). A volume on a stick comes from anyone, so the daemon
//! accepts a boot sector only when every figure agrees with the others and
//! with the device, using the rules of the Microsoft FAT specification:
//! the cluster count alone decides the FAT type, and the BPB fields of the
//! other types must be the values that type uses.

use crate::io::SECTOR;

/// What a valid boot sector says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// 12, 16 or 32.
    pub fat_bits: u8,
    pub sectors_per_cluster: u32,
    pub total_sectors: u64,
    pub clusters: u32,
    /// First sector after the reserved sectors and the FATs.
    pub fat_end: u64,
    /// First sector of the data area (cluster 2).
    pub data_start: u64,
}

const SIGNATURE: [u8; 2] = [0x55, 0xAA];
/// FAT12 holds fewer clusters than this, FAT16 fewer than [`FAT16_LIMIT`].
const FAT12_LIMIT: u64 = 4085;
const FAT16_LIMIT: u64 = 65525;
/// The largest cluster number a FAT32 entry can name.
const FAT32_MAX_CLUSTERS: u64 = 0x0FFF_FFF4;

fn le16(sector: &[u8], at: usize) -> u64 {
    u64::from(u16::from_le_bytes([sector[at], sector[at + 1]]))
}

fn le32(sector: &[u8], at: usize) -> u64 {
    u64::from(u32::from_le_bytes([
        sector[at],
        sector[at + 1],
        sector[at + 2],
        sector[at + 3],
    ]))
}

/// Check boot sector `sector` (the first 512 bytes of the volume) of a
/// volume on a device of `device_sectors`.
pub fn validate(sector: &[u8], device_sectors: u64) -> Result<Geometry, &'static str> {
    if sector.len() < SECTOR || sector[510..512] != SIGNATURE {
        return Err("no boot sector signature");
    }
    if le16(sector, 11) != SECTOR as u64 {
        return Err("sectors are not 512 bytes");
    }
    let per_cluster = u64::from(sector[13]);
    if !(1..=128).contains(&per_cluster) || !per_cluster.is_power_of_two() {
        return Err("bad sectors per cluster");
    }
    let reserved = le16(sector, 14);
    if reserved == 0 {
        return Err("no reserved sectors");
    }
    let fats = u64::from(sector[16]);
    if !(1..=2).contains(&fats) {
        return Err("bad FAT count");
    }
    let root_entries = le16(sector, 17);
    let total = match le16(sector, 19) {
        0 => le32(sector, 32),
        sectors => sectors,
    };
    if total == 0 || total > device_sectors {
        return Err("volume size does not fit the device");
    }
    let fat32 = le16(sector, 22) == 0;
    let per_fat = if fat32 {
        le32(sector, 36)
    } else {
        le16(sector, 22)
    };
    if per_fat == 0 {
        return Err("no FAT");
    }
    let root_sectors = (root_entries * 32).div_ceil(SECTOR as u64);
    if fat32 && root_entries != 0 {
        return Err("a FAT32 volume has no fixed root directory");
    }
    if !fat32 && (root_entries == 0 || (root_entries * 32) % SECTOR as u64 != 0) {
        return Err("bad root directory size");
    }
    let first_data = reserved + fats * per_fat + root_sectors;
    if first_data >= total {
        return Err("no data area");
    }
    let clusters = (total - first_data) / per_cluster;
    if clusters == 0 || clusters > FAT32_MAX_CLUSTERS {
        return Err("bad cluster count");
    }
    let fat_bits = if clusters < FAT12_LIMIT {
        12
    } else if clusters < FAT16_LIMIT {
        16
    } else {
        32
    };
    if (fat_bits == 32) != fat32 {
        return Err("cluster count and FAT type disagree");
    }
    // The table must hold an entry for every cluster (and the two reserved).
    let entries = clusters + 2;
    let capacity = per_fat * SECTOR as u64 * 8 / u64::from(fat_bits);
    if capacity < entries {
        return Err("FAT too small for its clusters");
    }
    if fat32 {
        if le16(sector, 42) != 0 {
            return Err("unsupported FAT32 version");
        }
        let root = le32(sector, 44);
        if !(2..entries).contains(&root) {
            return Err("root directory cluster out of range");
        }
        let info = le16(sector, 48);
        if info == 0 || info >= reserved {
            return Err("bad FSInfo sector");
        }
        let backup = le16(sector, 50);
        if backup != 0 && backup >= reserved {
            return Err("bad backup boot sector");
        }
    }
    Ok(Geometry {
        fat_bits,
        sectors_per_cluster: per_cluster as u32,
        total_sectors: total,
        clusters: clusters as u32,
        fat_end: reserved + fats * per_fat,
        data_start: first_data,
    })
}
