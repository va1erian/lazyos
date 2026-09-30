//! A builder for synthetic FAT12/16 images, shared by the long-name and
//! subdirectory suites (issue #414).
//!
//! The layout is fixed and simple: an MBR, one partition at LBA 1 with one
//! reserved sector, one FAT, a [`ROOT_ENTRIES`]-slot root region and one
//! sector per cluster. Directories are assembled from raw 32-byte slots, so a
//! test can hand-craft exactly the malformed long-name run or cluster chain it
//! wants to see the driver survive.

use crate::block::SECTOR_SIZE;
use crate::fs::fat::Fat16;
use crate::tests::block_suite::FakeDisk;
use alloc::vec;
use alloc::vec::Vec;

pub(super) const ROOT_ENTRIES: usize = 128;
const ROOT_SECTORS: usize = ROOT_ENTRIES * 32 / SECTOR_SIZE;
/// Directory slots per cluster (one sector per cluster).
pub(super) const SLOTS_PER_CLUSTER: usize = SECTOR_SIZE / 32;

pub(super) type Slot = [u8; 32];

pub(super) struct FatImage {
    pub data: Vec<u8>,
    fat16: bool,
    fat_sectors: usize,
    clusters: usize,
    next: u16,
}

fn put16(buf: &mut [u8], offset: usize, value: u16) {
    buf[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

impl FatImage {
    /// An empty image of `sectors` sectors (a FAT16 volume needs >= 4200).
    pub fn new(sectors: usize, fat16: bool) -> FatImage {
        let entry_bytes = if fat16 { 2 } else { 3 };
        let fat_bytes = if fat16 {
            (sectors + 2) * 2
        } else {
            ((sectors + 2) * entry_bytes).div_ceil(2)
        };
        let fat_sectors = fat_bytes.div_ceil(SECTOR_SIZE);
        let clusters = sectors - 1 - (1 + fat_sectors + ROOT_SECTORS);
        assert!(
            (clusters >= 4085) == fat16,
            "{clusters} clusters do not make the requested FAT type"
        );
        let mut img = FatImage {
            data: vec![0u8; sectors * SECTOR_SIZE],
            fat16,
            fat_sectors,
            clusters,
            next: 2,
        };
        // MBR entry 0.
        img.data[0x1BE + 4] = if fat16 { 0x06 } else { 0x01 };
        img.data[0x1BE + 8..0x1BE + 12].copy_from_slice(&1u32.to_le_bytes());
        img.data[0x1BE + 12..0x1BE + 16].copy_from_slice(&((sectors - 1) as u32).to_le_bytes());
        img.data[510] = 0x55;
        img.data[511] = 0xAA;
        // BPB at LBA 1.
        let bpb = SECTOR_SIZE;
        put16(&mut img.data, bpb + 11, SECTOR_SIZE as u16);
        img.data[bpb + 13] = 1;
        put16(&mut img.data, bpb + 14, 1);
        img.data[bpb + 16] = 1;
        put16(&mut img.data, bpb + 17, ROOT_ENTRIES as u16);
        put16(&mut img.data, bpb + 19, (sectors - 1) as u16);
        put16(&mut img.data, bpb + 22, fat_sectors as u16);
        img.set_fat(0, if fat16 { 0xFFF8 } else { 0xFF8 });
        img.set_fat(1, if fat16 { 0xFFFF } else { 0xFFF });
        img
    }

    fn fat_offset(&self) -> usize {
        2 * SECTOR_SIZE
    }

    fn root_offset(&self) -> usize {
        (2 + self.fat_sectors) * SECTOR_SIZE
    }

    fn cluster_offset(&self, cluster: u16) -> usize {
        self.root_offset() + (ROOT_SECTORS + usize::from(cluster) - 2) * SECTOR_SIZE
    }

    /// The end-of-chain value for this FAT width.
    pub fn eoc(&self) -> u16 {
        if self.fat16 {
            0xFFFF
        } else {
            0xFFF
        }
    }

    pub fn set_fat(&mut self, cluster: u16, value: u16) {
        let base = self.fat_offset();
        if self.fat16 {
            put16(&mut self.data, base + usize::from(cluster) * 2, value);
            return;
        }
        let offset = base + usize::from(cluster) + usize::from(cluster) / 2;
        let word = u16::from_le_bytes([self.data[offset], self.data[offset + 1]]);
        let word = if cluster.is_multiple_of(2) {
            (word & 0xF000) | (value & 0x0FFF)
        } else {
            (word & 0x000F) | ((value & 0x0FFF) << 4)
        };
        put16(&mut self.data, offset, word);
    }

    /// Claim the next free cluster as a one-cluster chain.
    pub fn alloc(&mut self) -> u16 {
        let cluster = self.next;
        assert!(
            usize::from(cluster) < self.clusters + 2,
            "the synthetic image is out of clusters"
        );
        self.next += 1;
        self.set_fat(cluster, self.eoc());
        cluster
    }

    /// Link `clusters` into one chain, in the order given.
    pub fn link(&mut self, clusters: &[u16]) {
        for pair in clusters.windows(2) {
            self.set_fat(pair[0], pair[1]);
        }
        if let Some(&last) = clusters.last() {
            self.set_fat(last, self.eoc());
        }
    }

    pub fn write_cluster(&mut self, cluster: u16, bytes: &[u8]) {
        let at = self.cluster_offset(cluster);
        self.data[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// Store a file and return its first cluster (`0` for an empty file).
    pub fn store_file(&mut self, bytes: &[u8]) -> u16 {
        let chunks: Vec<&[u8]> = bytes.chunks(SECTOR_SIZE).collect();
        let clusters: Vec<u16> = chunks.iter().map(|_| self.alloc()).collect();
        self.link(&clusters);
        for (cluster, chunk) in clusters.iter().zip(chunks) {
            self.write_cluster(*cluster, chunk);
        }
        clusters.first().copied().unwrap_or(0)
    }

    /// Store directory `slots` across `clusters` (linked in that order). The
    /// slots after the last one stay zero, which is the end marker.
    pub fn store_dir_in(&mut self, clusters: &[u16], slots: &[Slot]) {
        assert!(slots.len() <= clusters.len() * SLOTS_PER_CLUSTER);
        self.link(clusters);
        for (cluster, group) in clusters.iter().zip(slots.chunks(SLOTS_PER_CLUSTER)) {
            self.write_cluster(*cluster, &group.concat());
        }
    }

    /// Store directory `slots` in freshly allocated contiguous clusters.
    pub fn store_dir(&mut self, slots: &[Slot]) -> u16 {
        let count = slots.len().div_ceil(SLOTS_PER_CLUSTER).max(1);
        let clusters: Vec<u16> = (0..count).map(|_| self.alloc()).collect();
        self.store_dir_in(&clusters, slots);
        clusters[0]
    }

    /// Write the root directory region.
    pub fn set_root(&mut self, slots: &[Slot]) {
        assert!(slots.len() <= ROOT_ENTRIES, "root directory overflow");
        let at = self.root_offset();
        for (index, slot) in slots.iter().enumerate() {
            self.data[at + index * 32..at + index * 32 + 32].copy_from_slice(slot);
        }
    }

    /// Install into `disk` and open the volume.
    pub fn open(&self, disk: &'static FakeDisk) -> Fat16 {
        disk.data.lock().copy_from_slice(&self.data);
        Fat16::open(disk).expect("the synthetic FAT image should open")
    }

    /// A fresh fake disk called `name` holding this image, opened.
    pub fn mount(&self, name: &'static str) -> Fat16 {
        self.open(FakeDisk::new(name, self.data.len() / SECTOR_SIZE))
    }
}

/// An 11-byte space-padded short name from `"NAME.EXT"`.
pub(super) fn s83(name: &str) -> [u8; 11] {
    let (base, ext) = name.split_once('.').unwrap_or((name, ""));
    let mut out = [b' '; 11];
    out[..base.len()].copy_from_slice(base.as_bytes());
    out[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
    out
}

pub(super) const ATTR_FILE: u8 = 0x20;
pub(super) const ATTR_DIR: u8 = 0x10;

/// A short-name slot.
pub(super) fn short_slot(short: &[u8; 11], attr: u8, cluster: u16, size: u32) -> Slot {
    let mut slot = [0u8; 32];
    slot[..11].copy_from_slice(short);
    slot[11] = attr;
    slot[26..28].copy_from_slice(&cluster.to_le_bytes());
    slot[28..32].copy_from_slice(&size.to_le_bytes());
    slot
}

/// The `.` and `..` slots every real subdirectory starts with.
pub(super) fn dot_slots(own: u16, parent: u16) -> [Slot; 2] {
    [
        short_slot(b".          ", ATTR_DIR, own, 0),
        short_slot(b"..         ", ATTR_DIR, parent, 0),
    ]
}

/// The checksum a long-name run must carry for `short`.
pub(super) fn checksum(short: &[u8; 11]) -> u8 {
    short.iter().fold(0u8, |sum, &byte| {
        (sum >> 1).wrapping_add((sum & 1) << 7).wrapping_add(byte)
    })
}

/// A long-name run for raw UTF-16 `units`, first-stored slot first (the
/// highest sequence number, with the last flag). Pads with a terminator and
/// 0xFFFF only when the units do not fill the last slot exactly.
pub(super) fn lfn_run(units: &[u16], checksum: u8) -> Vec<Slot> {
    let mut padded = units.to_vec();
    if !padded.len().is_multiple_of(13) {
        padded.push(0);
        while !padded.len().is_multiple_of(13) {
            padded.push(0xFFFF);
        }
    }
    let count = padded.len() / 13;
    let mut run = Vec::new();
    for seq in (1..=count).rev() {
        let mut slot = [0u8; 32];
        slot[0] = seq as u8 | if seq == count { 0x40 } else { 0 };
        slot[11] = 0x0F;
        slot[13] = checksum;
        let chunk = &padded[(seq - 1) * 13..seq * 13];
        for (index, unit) in chunk.iter().enumerate() {
            let at = match index {
                0..=4 => 1 + index * 2,
                5..=10 => 14 + (index - 5) * 2,
                _ => 28 + (index - 11) * 2,
            };
            slot[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
        run.push(slot);
    }
    run
}

/// A long name's run followed by its short slot, all well-formed.
pub(super) fn long_entry(
    long: &str,
    short: &[u8; 11],
    attr: u8,
    cluster: u16,
    size: u32,
) -> Vec<Slot> {
    let units: Vec<u16> = long.encode_utf16().collect();
    let mut slots = lfn_run(&units, checksum(short));
    slots.push(short_slot(short, attr, cluster, size));
    slots
}
