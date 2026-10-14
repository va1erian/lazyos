//! The order of the writes of one operation (`io::order`): a cut after any
//! write must leave a volume whose files never claim more than the FAT
//! gives them, never share a cluster, and whose bytes are the bytes that
//! were written (or nothing yet).
//!
//! The checker reads the image itself, apart from `rust-fatfs`: FAT16
//! volumes, short entries of the root directory only (the scripts use
//! 8.3 names, upper case, in the root).

use alloc::string::String;
use alloc::vec::Vec;

use fused::daemon::FuseFs;

use super::*;
use crate::mem::MemDisk;

/// What the image's FAT16 volume says, read raw.
struct Raw<'a> {
    image: &'a [u8],
    fat: usize,
    root: usize,
    root_entries: usize,
    data: usize,
    cluster_bytes: usize,
    clusters: usize,
}

impl<'a> Raw<'a> {
    fn new(image: &'a [u8]) -> Raw<'a> {
        let le16 = |at: usize| usize::from(u16::from_le_bytes([image[at], image[at + 1]]));
        let per_cluster = usize::from(image[13]);
        let reserved = le16(14);
        let fats = usize::from(image[16]);
        let root_entries = le16(17);
        let per_fat = le16(22);
        let total = le16(19).max(1);
        let total = if le16(19) == 0 {
            u32::from_le_bytes([image[32], image[33], image[34], image[35]]) as usize
        } else {
            total
        };
        let root = (reserved + fats * per_fat) * 512;
        let data = root + root_entries * 32;
        let clusters = (total * 512 - data) / (per_cluster * 512);
        Raw {
            image,
            fat: reserved * 512,
            root,
            root_entries,
            data,
            cluster_bytes: per_cluster * 512,
            clusters,
        }
    }

    fn next(&self, cluster: usize) -> usize {
        let at = self.fat + cluster * 2;
        usize::from(u16::from_le_bytes([self.image[at], self.image[at + 1]]))
    }

    fn cluster(&self, cluster: usize) -> &[u8] {
        let at = self.data + (cluster - 2) * self.cluster_bytes;
        &self.image[at..at + self.cluster_bytes]
    }

    /// Every file of the root: name, size and its chain.
    fn files(&self) -> Result<Vec<(String, usize, Vec<usize>)>, String> {
        let mut found = Vec::new();
        for entry in 0..self.root_entries {
            let at = self.root + entry * 32;
            let e = &self.image[at..at + 32];
            if e[0] == 0 {
                break;
            }
            // Deleted, a long-name part, the volume label, a directory.
            if e[0] == 0xE5 || e[11] == 0x0F || e[11] & 0x08 != 0 || e[11] & 0x10 != 0 {
                continue;
            }
            let name = String::from_utf8_lossy(&e[..11]).trim_end().into();
            let first = usize::from(u16::from_le_bytes([e[26], e[27]]));
            let size = u32::from_le_bytes([e[28], e[29], e[30], e[31]]) as usize;
            let mut chain = Vec::new();
            let mut at = first;
            while at >= 2 && at < 0xFFF8 {
                if at >= self.clusters + 2 {
                    return Err(alloc::format!("{name}: cluster {at} out of range"));
                }
                if chain.len() > self.clusters {
                    return Err(alloc::format!("{name}: chain loops"));
                }
                chain.push(at);
                at = self.next(at);
            }
            if at == 1 || (0xFFF0..0xFFF8).contains(&at) {
                return Err(alloc::format!("{name}: chain ends in {at:#x}"));
            }
            if chain.contains(&0) {
                return Err(alloc::format!("{name}: chain through a free cluster"));
            }
            found.push((name, size, chain));
        }
        Ok(found)
    }
}

/// The volume is consistent after a cut: chains cover sizes, nothing is
/// shared, no chain runs into a free cluster.
fn consistent(raw: &Raw<'_>) -> Result<Vec<(String, usize, Vec<usize>)>, String> {
    let files = raw.files()?;
    let mut owner: alloc::collections::BTreeMap<usize, String> = Default::default();
    for (name, size, chain) in &files {
        if chain.len() * raw.cluster_bytes < *size {
            return Err(alloc::format!(
                "{name}: size {size} but {} clusters",
                chain.len()
            ));
        }
        for &cluster in chain {
            if raw.next(cluster) == 0 {
                return Err(alloc::format!("{name}: cluster {cluster} is free"));
            }
            if let Some(other) = owner.insert(cluster, name.clone()) {
                return Err(alloc::format!("{name} and {other} share cluster {cluster}"));
            }
        }
    }
    Ok(files)
}

fn content(raw: &Raw<'_>, size: usize, chain: &[usize]) -> Vec<u8> {
    let mut bytes: Vec<u8> = chain
        .iter()
        .flat_map(|&c| raw.cluster(c))
        .copied()
        .collect();
    bytes.truncate(size);
    bytes
}

/// Run `script` over a fresh formatted disk, cutting after `cut` writes.
fn cut_run(base: &MemDisk, cut: u64, script: &dyn Fn(&mut crate::Volume<MemDisk>)) -> Vec<u8> {
    let disk = MemDisk::from_image(&base.image());
    disk.fail_writes_after(cut);
    {
        let mut vol = open(&disk, opts());
        script(&mut vol);
    }
    disk.image()
}

/// How many device writes `script` makes uncut.
fn total_writes(base: &MemDisk, script: &dyn Fn(&mut crate::Volume<MemDisk>)) -> u64 {
    let disk = MemDisk::from_image(&base.image());
    let mut vol = open(&disk, opts());
    script(&mut vol);
    disk.writes()
}

#[test]
fn growing_a_file_never_leaves_an_entry_that_claims_unwritten_bytes() {
    // Small files go through the cache (no bypass), large ones are unaligned
    // pieces: both are flushed at the end of the operation, where the order
    // matters. Every file is a prefix of what was meant to be written, at
    // every cut.
    let want: [(&str, Vec<u8>); 3] = [
        ("A.TXT", pattern(3000, 1)),
        ("B.BIN", pattern(40_000, 2)),
        ("C.DAT", pattern(9000, 3)),
    ];
    let script = |vol: &mut crate::Volume<MemDisk>| {
        for (name, data) in &want {
            let _ = vol.create(name, 0o644, 0, 0);
            let mut at = 0;
            // Pieces that are not a multiple of the sector: partial sectors.
            for piece in data.chunks(7000) {
                if vol.write(path(name), at, piece).is_err() {
                    return;
                }
                at += piece.len() as u64;
            }
        }
        // Appending to an existing file.
        let _ = vol.write(path("A.TXT"), 3000, &pattern(2000, 9));
    };
    let base = MemDisk::formatted(SECTORS, 16, LABEL);
    let total = total_writes(&base, &script);
    assert!(total > 12, "{total}");
    let mut full = want.to_vec();
    full[0].1.extend_from_slice(&pattern(2000, 9));
    let mut tested = 0;
    for cut in 0..=total {
        let image = cut_run(&base, cut, &script);
        let raw = Raw::new(&image);
        let files = consistent(&raw).unwrap_or_else(|why| panic!("cut {cut}: {why}"));
        for (name, size, chain) in files {
            let wanted = full
                .iter()
                .find(|(n, _)| {
                    let (stem, ext) = n.split_once('.').unwrap();
                    name == alloc::format!("{stem:<8}{ext:<3}")
                })
                .map(|(_, d)| d)
                .unwrap_or_else(|| panic!("cut {cut}: unknown file {name:?}"));
            let got = content(&raw, size, &chain);
            assert!(
                got.len() <= wanted.len() && got[..] == wanted[..got.len()],
                "cut {cut}: {name:?} claims {size} bytes and they are not what was written"
            );
            tested += 1;
        }
    }
    assert!(tested > 20, "{tested} file states checked");
}

#[test]
fn shrinking_and_deleting_never_leave_an_entry_over_freed_clusters() {
    // A file is cut down and deleted, and another takes the freed clusters.
    // At every cut no entry may claim more than its chain, name a free
    // cluster, or share one with another file.
    let script = |vol: &mut crate::Volume<MemDisk>| {
        let _ = vol.create("OLD.BIN", 0o644, 0, 0);
        let _ = vol.write(path("OLD.BIN"), 0, &pattern(30_000, 4));
        let _ = vol.create("KEEP.BIN", 0o644, 0, 0);
        let _ = vol.write(path("KEEP.BIN"), 0, &pattern(10_000, 5));
        let _ = vol.truncate(path("OLD.BIN"), 1000);
        let _ = vol.create("NEW.BIN", 0o644, 0, 0);
        let _ = vol.write(path("NEW.BIN"), 0, &pattern(25_000, 6));
        let _ = vol.unlink("OLD.BIN");
        let _ = vol.create("LAST.BIN", 0o644, 0, 0);
        let _ = vol.write(path("LAST.BIN"), 0, &pattern(20_000, 7));
        // Replacing a file frees its clusters too.
        let _ = vol.rename("NEW.BIN", "KEEP.BIN");
    };
    let base = MemDisk::formatted(SECTORS, 16, LABEL);
    let total = total_writes(&base, &script);
    assert!(total > 12, "{total}");
    for cut in 0..=total {
        let image = cut_run(&base, cut, &script);
        let raw = Raw::new(&image);
        if let Err(why) = consistent(&raw) {
            panic!("cut {cut} of {total}: {why}");
        }
    }
}

#[test]
fn the_old_ascending_order_is_what_the_checker_catches() {
    // The checker is only worth something if it fails on the order this
    // module replaced: write the same shrink with the cache's default
    // (ascending) order and find a cut that leaves the volume inconsistent.
    let script = |vol: &mut crate::Volume<MemDisk>| {
        let _ = vol.create("OLD.BIN", 0o644, 0, 0);
        let _ = vol.write(path("OLD.BIN"), 0, &pattern(30_000, 4));
        // The truncate, written ascending: the FAT frees before the entry
        // shrinks.
        vol.set_write_ordering(false);
        let _ = vol.truncate(path("OLD.BIN"), 1000);
    };
    let base = MemDisk::formatted(SECTORS, 16, LABEL);
    let total = total_writes(&base, &script);
    let broken = (0..=total)
        .filter(|&cut| {
            let image = cut_run(&base, cut, &script);
            consistent(&Raw::new(&image)).is_err()
        })
        .count();
    assert!(broken > 0, "no cut of the ascending order was caught");
}
