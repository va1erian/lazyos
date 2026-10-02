//! Sustained load: 1000 create/update cycles over a 300-file tree at random
//! sizes, then the fsck-style invariants (bitmaps against reachable blocks,
//! free counters, link counts) over the raw bytes.

use fuzzkit::Rng;

use super::*;
use crate::Owner;

const FILES: usize = 300;
const DIRS: usize = 10;
const CYCLES: usize = 1000;

/// A size drawn so most files are small, some use the single-indirect range
/// and a few reach the double-indirect range (1 KiB blocks: past 268 KiB).
fn random_size(rng: &mut Rng) -> usize {
    match rng.below(100) {
        0..=69 => rng.below(4096) as usize,
        70..=94 => rng.below(40_000) as usize,
        _ => 270_000 + rng.below(60_000) as usize,
    }
}

fn contents(rng: &mut Rng, len: usize) -> Vec<u8> {
    rng.bytes(len)
}

fn path_of(index: usize) -> String {
    std::format!("/d{}/file-{index:03}", index % DIRS)
}

/// One cycle on file `index`: create it, or update it one of several ways.
fn cycle(fs: &Ext2, rng: &mut Rng, index: usize, model: &mut [Option<Vec<u8>>]) {
    let path = path_of(index);
    let Some(current) = model[index].as_mut() else {
        let len = random_size(rng);
        let data = contents(rng, len);
        fs.write_file(&path, &data, 0o644, 1000, 1000, 5).unwrap();
        model[index] = Some(data);
        return;
    };
    match rng.below(10) {
        0 => {
            fs.unlink(&path).unwrap();
            model[index] = None;
        }
        1..=3 => {
            let len = random_size(rng);
            let data = contents(rng, len);
            fs.write_file(&path, &data, 0o644, 1000, 1000, 6).unwrap();
            *current = data;
        }
        4..=5 => {
            let size = random_size(rng);
            fs.truncate(&path, size as u64).unwrap();
            current.resize(size, 0);
        }
        6..=7 => {
            let offset = rng.below(current.len() as u64 + 2000) as usize;
            let patch_len = rng.below(5000) as usize;
            let patch = contents(rng, patch_len);
            assert_eq!(fs.write(&path, offset as u64, &patch).unwrap(), patch.len());
            if current.len() < offset + patch.len() {
                current.resize(offset + patch.len(), 0);
            }
            current[offset..offset + patch.len()].copy_from_slice(&patch);
        }
        8 => {
            let target = path_of((index + DIRS) % FILES);
            if model[(index + DIRS) % FILES].is_none() {
                fs.rename(&path, &target).unwrap();
                model[(index + DIRS) % FILES] = model[index].take();
            }
        }
        _ => {
            let mut head = [0u8; 512];
            let read = fs.read(&path, 0, &mut head).unwrap();
            assert_eq!(&head[..read], &current[..read.min(current.len())]);
        }
    }
}

fn verify(fs: &Ext2, model: &[Option<Vec<u8>>]) {
    for (index, want) in model.iter().enumerate() {
        let path = path_of(index);
        match want {
            Some(bytes) => assert_eq!(&fs.read_file(&path).unwrap(), bytes, "{path}"),
            None => assert!(fs.lookup(&path).is_err(), "{path} should be gone"),
        }
    }
}

#[test]
fn a_thousand_update_cycles_over_three_hundred_files() {
    let (io, fs) = fresh(48 * 1024 * 1024, 1024);
    let mut rng = Rng::new(0x5EED_CAFE);
    for dir in 0..DIRS {
        fs.mkdir(&std::format!("/d{dir}"), 0o755, Owner::ROOT)
            .unwrap();
    }
    let mut model: Vec<Option<Vec<u8>>> = std::vec![None; FILES];
    // Populate the whole tree first, then churn it.
    for index in 0..FILES {
        cycle(&fs, &mut rng, index, &mut model);
    }
    assert_clean(&io);
    for round in 0..CYCLES {
        let index = rng.below(FILES as u64) as usize;
        cycle(&fs, &mut rng, index, &mut model);
        if round % 125 == 124 {
            assert_clean(&io); // the invariants hold mid-run, not just at the end
        }
    }
    verify(&fs, &model);
    fs.flush().unwrap();
    drop(fs);
    assert_clean(&io);
    verify(&open(&io), &model);

    // Deleting everything must return every block and inode.
    let fs = open(&io);
    let baseline = formatted(48 * 1024 * 1024, 1024);
    for dir in 0..DIRS {
        fs.remove_tree(&std::format!("/d{dir}")).unwrap();
    }
    let empty = open(&baseline);
    assert_eq!(fs.free_blocks().unwrap(), empty.free_blocks().unwrap());
    assert_eq!(fs.free_inodes().unwrap(), empty.free_inodes().unwrap());
    fs.flush().unwrap();
    assert_clean(&io);
}

#[test]
fn heavy_churn_in_a_small_volume_never_leaks() {
    // A nearly full volume: allocation failures are routine, and every one of
    // them must unwind without leaking a block or inode.
    let (io, fs) = fresh(2 * 1024 * 1024, 1024);
    let mut rng = Rng::new(42);
    for round in 0..600 {
        let path = std::format!("/f{}", rng.below(40));
        let len = rng.below(120_000) as usize;
        let data = contents(&mut rng, len);
        match fs.write_file(&path, &data, 0o644, 0, 0, round) {
            Ok(_) => assert_eq!(fs.read_file(&path).unwrap(), data),
            Err(error) => {
                assert_eq!(error, crate::Ext2Error::NoSpace);
                let _ = fs.unlink(&path);
            }
        }
    }
    assert_clean(&io);
}
