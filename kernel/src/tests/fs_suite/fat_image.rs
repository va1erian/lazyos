//! Synthetic FAT12 volume builder for the long-name and subdirectory tests
//! (issue #414): one sector per cluster, hand-written directory slots and
//! LFN fragments, so each test can damage exactly one thing.

use super::fat_corruption::{put16, put32, set_fat12};
use super::*;
use crate::block::SECTOR_SIZE;
use crate::fs::fat::Fat16;
use crate::fs::vfs::Filesystem;
use crate::tests::block_suite::FakeDisk;
use alloc::vec;
use alloc::vec::Vec;

pub(super) const ROOT_SLOTS: usize = 64;
pub(super) const FAT_SECTORS: usize = 2;
pub(super) const ROOT_LBA: usize = 1 + FAT_SECTORS;
pub(super) const DATA_LBA: usize = ROOT_LBA + ROOT_SLOTS / 16;
/// One sector per cluster; cluster 2 is the first data sector.
pub(super) const CLUSTERS: usize = 700;
pub(super) const TOTAL_SECTORS: usize = DATA_LBA + CLUSTERS;
pub(super) const ATTR_DIR: u8 = 0x10;
pub(super) const ATTR_FILE: u8 = 0x20;

/// A synthetic bare FAT12 volume (BPB in sector 0, one sector per cluster).
pub(super) struct Vol {
    pub(super) img: Vec<u8>,
    /// Next never-used cluster.
    pub(super) next: u16,
}

/// Where a directory's next slot goes.
pub(super) struct Dir {
    /// Byte offsets of every slot the directory can hold.
    pub(super) slots: Vec<usize>,
    pub(super) used: usize,
}

impl Vol {
    pub(super) fn new() -> Vol {
        let mut img = vec![0u8; TOTAL_SECTORS * SECTOR_SIZE];
        put16(&mut img, 11, SECTOR_SIZE as u16);
        img[13] = 1;
        put16(&mut img, 14, 1);
        img[16] = 1;
        put16(&mut img, 17, ROOT_SLOTS as u16);
        put16(&mut img, 19, TOTAL_SECTORS as u16);
        put16(&mut img, 22, FAT_SECTORS as u16);
        img[510] = 0x55;
        img[511] = 0xAA;
        let mut vol = Vol { img, next: 2 };
        vol.fat(0, 0xFF8);
        vol.fat(1, 0xFFF);
        vol
    }

    pub(super) fn fat(&mut self, cluster: u16, value: u16) {
        let fat = SECTOR_SIZE;
        set_fat12(
            &mut self.img[fat..fat + FAT_SECTORS * SECTOR_SIZE],
            cluster,
            value,
        );
    }

    pub(super) fn cluster_off(cluster: u16) -> usize {
        (DATA_LBA + cluster as usize - 2) * SECTOR_SIZE
    }

    /// Chain `clusters` together (last one ends the chain).
    pub(super) fn link(&mut self, clusters: &[u16]) {
        for pair in clusters.windows(2) {
            self.fat(pair[0], pair[1]);
        }
        if let Some(&last) = clusters.last() {
            self.fat(last, 0xFFF);
        }
    }

    pub(super) fn alloc(&mut self, count: usize) -> Vec<u16> {
        let out: Vec<u16> = (self.next..self.next + count as u16).collect();
        self.next += count as u16;
        self.link(&out);
        out
    }

    /// A file with `data` in fresh contiguous clusters; returns its start.
    pub(super) fn file_data(&mut self, data: &[u8]) -> u16 {
        let clusters = self.alloc(data.len().div_ceil(SECTOR_SIZE).max(1));
        for (i, chunk) in data.chunks(SECTOR_SIZE).enumerate() {
            let at = Self::cluster_off(clusters[i]);
            self.img[at..at + chunk.len()].copy_from_slice(chunk);
        }
        clusters[0]
    }

    pub(super) fn root(&self) -> Dir {
        Dir {
            slots: (0..ROOT_SLOTS)
                .map(|i| ROOT_LBA * SECTOR_SIZE + i * 32)
                .collect(),
            used: 0,
        }
    }

    /// A directory over the given (already chosen) clusters, linked in order.
    pub(super) fn dir_over(&mut self, clusters: &[u16]) -> Dir {
        self.link(clusters);
        Dir {
            slots: clusters
                .iter()
                .flat_map(|&c| (0..16).map(move |i| Self::cluster_off(c) + i * 32))
                .collect(),
            used: 0,
        }
    }

    /// Write raw 32-byte slots, returning the offset of the last one.
    pub(super) fn put_slots(&mut self, dir: &mut Dir, slots: &[[u8; 32]]) -> usize {
        let mut at = 0;
        for slot in slots {
            at = dir.slots[dir.used];
            self.img[at..at + 32].copy_from_slice(slot);
            dir.used += 1;
        }
        at
    }

    /// Add `short` (11 bytes) with optional long name; returns the short
    /// entry's byte offset.
    pub(super) fn add(
        &mut self,
        dir: &mut Dir,
        long: Option<&str>,
        short: &[u8; 11],
        attr: u8,
        cluster: u16,
        size: u32,
    ) -> usize {
        let mut slots = long.map(|name| lfn_slots(name, short)).unwrap_or_default();
        slots.push(short_slot(short, attr, cluster, size));
        self.put_slots(dir, &slots)
    }
}

pub(super) fn short_slot(short: &[u8; 11], attr: u8, cluster: u16, size: u32) -> [u8; 32] {
    let mut slot = [0u8; 32];
    slot[..11].copy_from_slice(short);
    slot[11] = attr;
    put16(&mut slot, 26, cluster);
    put32(&mut slot, 28, size);
    slot
}

pub(super) fn checksum(short: &[u8; 11]) -> u8 {
    short.iter().fold(0u8, |sum, &b| {
        (if sum & 1 != 0 { 0x80u8 } else { 0 })
            .wrapping_add(sum >> 1)
            .wrapping_add(b)
    })
}

/// LFN fragments for `name` in on-disk order (last fragment first).
pub(super) fn lfn_slots(name: &str, short: &[u8; 11]) -> Vec<[u8; 32]> {
    let units: Vec<u16> = name.encode_utf16().collect();
    lfn_slots_units(&units, checksum(short))
}

pub(super) fn lfn_slots_units(units: &[u16], sum: u8) -> Vec<[u8; 32]> {
    let mut padded = units.to_vec();
    if padded.len() % 13 != 0 {
        padded.push(0);
        while padded.len() % 13 != 0 {
            padded.push(0xFFFF);
        }
    }
    let frags = padded.len() / 13;
    let mut out = Vec::new();
    for ord in (1..=frags).rev() {
        let mut slot = [0u8; 32];
        slot[0] = ord as u8 | if ord == frags { 0x40 } else { 0 };
        slot[11] = 0x0F;
        slot[13] = sum;
        let chunk = &padded[(ord - 1) * 13..ord * 13];
        let offsets = (1..11)
            .step_by(2)
            .chain((14..26).step_by(2))
            .chain((28..32).step_by(2));
        for (unit, at) in chunk.iter().zip(offsets) {
            put16(&mut slot, at, *unit);
        }
        out.push(slot);
    }
    out
}

pub(super) fn open(vol: &Vol, name: &'static str) -> Fat16 {
    let disk = FakeDisk::new(name, TOTAL_SECTORS);
    disk.data.lock().copy_from_slice(&vol.img);
    Fat16::open(disk).expect("the synthetic FAT image should open")
}

pub(super) fn names(fs: &Fat16, path: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = fs
        .readdir(path)
        .map_err(|e| format!("readdir {path}: {e:?}"))?
        .into_iter()
        .map(|e| e.name)
        .collect();
    out.sort();
    Ok(out)
}

pub(super) fn read_all(fs: &Fat16, path: &str) -> Result<Vec<u8>, String> {
    let size = fs
        .lookup(path)
        .map_err(|e| format!("lookup {path}: {e:?}"))?
        .size as usize;
    let mut buf = vec![0u8; size];
    let got = fs
        .read(path, 0, &mut buf)
        .map_err(|e| format!("read {path}: {e:?}"))?;
    buf.truncate(got);
    Ok(buf)
}

/// A bare FAT12 image with one root file, for suites outside `fs_suite` that
/// need a boot volume (the mount suite's `lazyos.cfg`).
pub(in crate::tests) fn image_with_file(short: &[u8; 11], data: &[u8]) -> Vec<u8> {
    let mut vol = Vol::new();
    let cluster = vol.file_data(data);
    let mut root = vol.root();
    vol.add(
        &mut root,
        None,
        short,
        ATTR_FILE,
        cluster,
        data.len() as u32,
    );
    vol.img
}
