//! Cluster-chain reads for the FAT12/16 volume (issues #235, #244).
//!
//! Every helper here reads through [`Fat16`]'s own device handle and treats
//! the on-disk FAT as attacker-controlled: chain pointers are range-checked,
//! walks are capped at the volume's cluster count, and a directory entry's
//! size is bounded by the bytes the clusters can actually hold.

use super::{Fat16, FatKind};
use crate::block::SECTOR_SIZE;

/// The outcome of following one FAT entry; see [`Fat16::chain_step`].
pub(super) enum Step {
    Next(u16),
    End,
    Bad,
}

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
    pub(super) fn cluster_lba(&self, cluster: u16) -> Option<u32> {
        let index = (cluster as u32).checked_sub(2)?;
        if index >= self.clusters {
            return None;
        }
        let offset = index.checked_mul(u32::from(self.sectors_per_cluster))?;
        self.data_lba.checked_add(offset)
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
    fn fat_sector(&self, lba: u32) -> Option<[u8; SECTOR_SIZE]> {
        let mut cache = self.fat_cache.lock();
        if let Some((cached, data)) = cache.as_ref() {
            if *cached == lba {
                return Some(*data);
            }
        }
        let data = self.read_sector(lba)?;
        *cache = Some((lba, data));
        Some(data)
    }

    /// Next cluster in a chain, following the FAT (12- or 16-bit entries).
    ///
    /// Returns `None` for the end-of-chain markers, but also for every value
    /// that is not a plausible data cluster: `0`/`1`, the bad-cluster marker,
    /// and anything past the volume's last cluster. Treating those as an
    /// error stops a corrupt image from steering a read into the FAT or a
    /// neighbouring partition (issue #235).
    fn next_cluster(&self, cluster: u16) -> Option<u16> {
        match self.chain_step(cluster) {
            Step::Next(next) => Some(next),
            Step::End | Step::Bad => None,
        }
    }

    /// One step along a chain, telling a clean end apart from corruption.
    ///
    /// File reads treat both as "no next cluster"; directory walks need the
    /// difference, because a directory that ends cleanly is complete while a
    /// bad pointer means the listing cannot be trusted.
    pub(super) fn chain_step(&self, cluster: u16) -> Step {
        if cluster < 2 || u32::from(cluster) > self.clusters + 1 {
            return Step::Bad;
        }
        let byte_offset = match self.kind {
            FatKind::Fat12 => cluster as u32 + cluster as u32 / 2,
            FatKind::Fat16 => cluster as u32 * 2,
        };
        let Some(sector) = self
            .fat_start
            .checked_add(byte_offset / self.bytes_per_sector as u32)
        else {
            return Step::Bad;
        };
        let index = (byte_offset % self.bytes_per_sector as u32) as usize;

        let Some(word) = self.read_fat_word(sector, index) else {
            return Step::Bad;
        };
        let (value, bad, end_of_chain) = match self.kind {
            // 12-bit entries are packed; pick the low or high nibble pair.
            FatKind::Fat12 => {
                let value = if cluster.is_multiple_of(2) {
                    word & 0x0FFF
                } else {
                    word >> 4
                };
                (value, value == 0xFF7, value >= 0xFF8)
            }
            FatKind::Fat16 => (word, word == 0xFFF7, word >= 0xFFF8),
        };
        if end_of_chain {
            return Step::End;
        }
        // `0` (free), `1`, the bad-cluster marker, other reserved markers and
        // out-of-range values are all corruption in the middle of a chain.
        if bad || value < 2 || u32::from(value) > self.clusters + 1 {
            return Step::Bad;
        }
        Step::Next(value)
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
            // Extend the run over clusters that follow on disk (the image
            // builder lays files out contiguously), so one device command
            // covers many clusters instead of one per cluster.
            let want = remaining - written;
            let mut span = cluster_bytes as usize - inner;
            let mut last = cluster;
            let mut following = None;
            while span < want {
                steps += 1;
                if steps > self.clusters {
                    return None; // cyclic or over-long chain
                }
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
            self.read_span(lba, inner, &mut buf[written..written + take])?;
            written += take;
            if written < remaining {
                steps += 1;
                if steps > self.clusters {
                    return None;
                }
                cluster = following?;
                inner = 0;
            }
        }
        Some(written)
    }

    /// Fill `out` from the byte range starting `byte` bytes into the run of
    /// sectors that begins at `lba`. Whole sectors go straight into `out` in
    /// one device command; only a partial head or tail sector uses a bounce.
    fn read_span(&self, lba: u32, byte: usize, out: &mut [u8]) -> Option<()> {
        let sector_bytes = self.bytes_per_sector as usize;
        let mut next = lba.checked_add((byte / sector_bytes) as u32)?;
        let head = byte % sector_bytes;
        let mut rest = out;
        if head != 0 || rest.len() < sector_bytes {
            let sector = self.read_sector(next)?;
            let take = (sector_bytes - head).min(rest.len());
            let (now, later) = rest.split_at_mut(take);
            now.copy_from_slice(&sector[head..head + take]);
            rest = later;
            next = next.checked_add(1)?;
        }
        let whole = rest.len() / sector_bytes * sector_bytes;
        if whole > 0 {
            let (now, later) = rest.split_at_mut(whole);
            self.device.read_sectors(u64::from(next), now).ok()?;
            rest = later;
            next = next.checked_add((whole / sector_bytes) as u32)?;
        }
        if !rest.is_empty() {
            let sector = self.read_sector(next)?;
            let tail = rest.len();
            rest.copy_from_slice(&sector[..tail]);
        }
        Some(())
    }
}
