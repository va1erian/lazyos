//! Cluster-chain reads for the FAT12/16 volume (issues #235, #244).
//!
//! Every helper here reads through [`Fat16`]'s own device handle and treats
//! the on-disk FAT as attacker-controlled: chain pointers are range-checked,
//! walks are capped at the volume's cluster count, and a directory entry's
//! size is bounded by the bytes the clusters can actually hold.

use super::{Fat16, FatKind};
use crate::block::SECTOR_SIZE;

impl Fat16 {
    /// Read one 512-byte sector from this volume's device.
    pub(super) fn read_sector(&self, lba: u32) -> Option<[u8; SECTOR_SIZE]> {
        let mut buf = [0u8; SECTOR_SIZE];
        self.device.read_sectors(u64::from(lba), &mut buf).ok()?;
        Some(buf)
    }

    /// Total bytes the volume's clusters can hold: the hard cap on any file
    /// size a chain can back.
    pub(super) fn capacity_bytes(&self) -> u64 {
        u64::from(self.clusters)
            * u64::from(self.sectors_per_cluster)
            * u64::from(self.bytes_per_sector)
    }

    /// LBA of a data cluster's first sector. `None` for the reserved cluster
    /// values `0`/`1` or an out-of-range cluster (issue #235).
    fn cluster_lba(&self, cluster: u16) -> Option<u32> {
        let index = (cluster as u32).checked_sub(2)?;
        if index >= self.clusters {
            return None;
        }
        let offset = index.checked_mul(u32::from(self.sectors_per_cluster))?;
        self.data_lba.checked_add(offset)
    }

    /// Read a little-endian 16-bit FAT entry that may straddle a sector edge.
    fn read_fat_word(&self, sector: u32, index: usize) -> Option<u16> {
        let buf = self.read_sector(sector)?;
        let low = buf[index] as u16;
        let high = if index + 1 < self.bytes_per_sector as usize {
            buf[index + 1] as u16
        } else {
            self.read_sector(sector + 1)?[0] as u16
        };
        Some(low | (high << 8))
    }

    /// Next cluster in a chain, following the FAT (12- or 16-bit entries).
    ///
    /// Returns `None` for the end-of-chain markers, but also for every value
    /// that is not a plausible data cluster: `0`/`1`, the bad-cluster marker,
    /// and anything past the volume's last cluster. Treating those as an
    /// error stops a corrupt image from steering a read into the FAT or a
    /// neighbouring partition (issue #235).
    fn next_cluster(&self, cluster: u16) -> Option<u16> {
        if cluster < 2 || u32::from(cluster) > self.clusters + 1 {
            return None;
        }
        let byte_offset = match self.kind {
            FatKind::Fat12 => cluster as u32 + cluster as u32 / 2,
            FatKind::Fat16 => cluster as u32 * 2,
        };
        let sector = self
            .fat_start
            .checked_add(byte_offset / self.bytes_per_sector as u32)?;
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

        let (bad, end_of_chain) = match self.kind {
            FatKind::Fat12 => (value == 0xFF7, value >= 0xFF8),
            FatKind::Fat16 => (value == 0xFFF7, value >= 0xFFF8),
        };
        // `0` and the EOC markers end the chain; `1`, the bad-cluster marker,
        // and out-of-range values are invalid and end it too (as an error at
        // the caller). Reserved markers (0xFF0..0xFFF6 / 0xFF0..0xFF6) land in
        // the range check below.
        if end_of_chain || value == 0 {
            return None;
        }
        if bad || value < 2 || u32::from(value) > self.clusters + 1 {
            return None;
        }
        Some(value)
    }

    /// Read `buf.len()` bytes at `offset` without loading the whole chain: walk
    /// to the first cluster, then read only the sectors the range touches.
    ///
    /// Every chain walk is capped at the volume's cluster count, so a looping
    /// or absurdly long chain cannot spin with interrupts off (issue #235).
    pub(super) fn read_at(
        &self,
        start: u16,
        size: u32,
        offset: u64,
        buf: &mut [u8],
    ) -> Option<usize> {
        if offset >= size as u64 || start < 2 {
            return Some(0);
        }
        let cluster_bytes = self.sectors_per_cluster as u64 * self.bytes_per_sector as u64;
        let sector_bytes = self.bytes_per_sector as usize;
        let mut cluster = start;
        let mut skip = offset / cluster_bytes;
        if skip > u64::from(self.clusters) {
            return None;
        }
        while skip > 0 {
            cluster = self.next_cluster(cluster)?;
            skip -= 1;
        }

        let mut inner = (offset % cluster_bytes) as usize;
        let remaining = (size as u64 - offset).min(buf.len() as u64) as usize;
        let mut written = 0usize;
        let mut steps = 0u32;
        while written < remaining {
            let lba = self.cluster_lba(cluster)?;
            let mut sector = (inner / sector_bytes) as u32;
            let mut byte = inner % sector_bytes;
            while sector < self.sectors_per_cluster as u32 && written < remaining {
                let data = self.read_sector(lba + sector)?;
                let take = (sector_bytes - byte).min(remaining - written);
                buf[written..written + take].copy_from_slice(&data[byte..byte + take]);
                written += take;
                byte = 0;
                sector += 1;
            }
            if written < remaining {
                steps += 1;
                if steps > self.clusters {
                    return None; // cyclic or over-long chain
                }
                cluster = self.next_cluster(cluster)?;
                inner = 0;
            }
        }
        Some(written)
    }
}
