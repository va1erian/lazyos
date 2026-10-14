//! A long random run against a model of the tree, with the volume settled
//! and mounted again every so often.

use std::collections::{BTreeMap, BTreeSet};
use std::string::String;

use fused::daemon::FuseFs;

use super::*;
use crate::mem::MemDisk;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[derive(Default)]
struct Model {
    /// Lower-cased path to bytes.
    files: BTreeMap<String, Vec<u8>>,
    /// Lower-cased directory paths (the root is "").
    dirs: BTreeSet<String>,
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        String::from(name)
    } else {
        alloc::format!("{dir}/{name}")
    }
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

impl Model {
    fn is_empty_dir(&self, dir: &str) -> bool {
        let prefix = alloc::format!("{dir}/");
        !self.files.keys().any(|k| k.starts_with(&prefix))
            && !self.dirs.iter().any(|d| d.starts_with(&prefix))
    }

    fn exists(&self, path: &str) -> bool {
        self.files.contains_key(path) || self.dirs.contains(path)
    }
}

fn check(vol: &mut Volume<MemDisk>, model: &Model, step: usize) {
    // Every model entry exists with the right content...
    for (path_of, bytes) in &model.files {
        let got = get(vol, path_of).unwrap_or_else(|code| panic!("step {step}: {path_of}: {code}"));
        assert!(got == *bytes, "step {step}: {path_of} differs");
    }
    // ...and the volume holds nothing else.
    fn walk(vol: &mut Volume<MemDisk>, dir: &str, out: &mut BTreeSet<String>) {
        for entry in vol.readdir(dir).unwrap() {
            let full = join(dir, &entry.name.to_lowercase());
            out.insert(full.clone());
            if entry.dir {
                walk(vol, &full, out);
            }
        }
    }
    let mut seen = BTreeSet::new();
    walk(vol, "", &mut seen);
    let expected: BTreeSet<String> = model
        .files
        .keys()
        .chain(model.dirs.iter().filter(|d| !d.is_empty()))
        .cloned()
        .collect();
    assert_eq!(seen, expected, "step {step}: tree differs");
}

fn run(fat: u8, seed: u64, steps: usize) {
    let sectors = if fat == 32 { SECTORS_32 } else { SECTORS };
    let disk = MemDisk::formatted(sectors, fat, LABEL);
    let mut vol = open(&disk, opts());
    let mut model = Model::default();
    model.dirs.insert(String::new());
    let mut rng = Rng(seed | 1);
    let names = [
        "alpha",
        "beta",
        "gamma",
        "delta",
        "ep sil on.txt",
        "Zeta.DAT",
        "eta",
    ];
    for step in 0..steps {
        let dirs: Vec<String> = model.dirs.iter().cloned().collect();
        let dir = dirs[rng.below(dirs.len())].clone();
        let name = alloc::format!("{}{}", names[rng.below(names.len())], rng.below(6));
        let target = join(&dir, &name.to_lowercase());
        let files: Vec<String> = model.files.keys().cloned().collect();
        match rng.below(10) {
            0 | 1 if dir.matches('/').count() < 3 && model.files.len() + model.dirs.len() < 80 => {
                let result = if rng.below(4) == 0 {
                    vol.mkdir(&join(&dir, &name), 0o755, 0, 0).map(|_| ())
                } else {
                    vol.create(&join(&dir, &name), 0o644, 0, 0).map(|_| ())
                };
                if model.exists(&target) {
                    assert_eq!(result.err(), Some(e::EEXIST), "step {step}: {target}");
                } else {
                    result.unwrap_or_else(|code| panic!("step {step}: create {target}: {code}"));
                    // What the last arm of the match above decided is not
                    // known here: ask the volume.
                    if vol.lookup(path(&join(&dir, &name))).unwrap().is_dir() {
                        model.dirs.insert(target);
                    } else {
                        model.files.insert(target, Vec::new());
                    }
                }
            }
            2..=4 if !files.is_empty() => {
                let file = files[rng.below(files.len())].clone();
                let size = model.files[&file].len();
                let at = rng.below(size + 1500);
                let data = pattern(1 + rng.below(20_000), rng.next() as u8);
                vol.write(path(&file), at as u64, &data).unwrap();
                let bytes = model.files.get_mut(&file).unwrap();
                if bytes.len() < at + data.len() {
                    bytes.resize(at + data.len(), 0);
                }
                bytes[at..at + data.len()].copy_from_slice(&data);
            }
            5 if !files.is_empty() => {
                let file = files[rng.below(files.len())].clone();
                let size = rng.below(model.files[&file].len() + 3000);
                vol.truncate(path(&file), size as u64).unwrap();
                model.files.get_mut(&file).unwrap().resize(size, 0);
            }
            6 if !files.is_empty() => {
                let file = files[rng.below(files.len())].clone();
                vol.unlink(&file).unwrap();
                model.files.remove(&file);
            }
            7 if dirs.len() > 1 => {
                let victim = dirs[1 + rng.below(dirs.len() - 1)].clone();
                let result = vol.rmdir(&victim);
                if model.is_empty_dir(&victim) {
                    result.unwrap();
                    model.dirs.remove(&victim);
                } else {
                    assert_eq!(result.err(), Some(e::ENOTEMPTY), "step {step}: {victim}");
                }
            }
            8 if !files.is_empty() => {
                let file = files[rng.below(files.len())].clone();
                let to = join(&dir, &name.to_lowercase());
                if model.dirs.contains(&to) || to == file {
                    continue;
                }
                vol.rename(&file, &to).unwrap();
                let bytes = model.files.remove(&file).unwrap();
                model.files.insert(to, bytes);
            }
            9 if step % 7 == 0 => {
                vol.settle().unwrap();
                drop(vol);
                vol = open(&disk, opts());
                check(&mut vol, &model, step);
            }
            _ => {}
        }
        let _ = parent("");
    }
    check(&mut vol, &model, steps);
    vol.settle().unwrap();
}

#[test]
fn random_operations_match_the_model_on_every_fat_type() {
    for fat in [12, 16, 32] {
        for seed in [0x1234_5678_9ABC_DEF1u64, 0xFEED_FACE_0BAD_F00D] {
            run(fat, seed, 600);
        }
    }
}

#[test]
#[ignore = "a long soak: cargo test -p fatvol --release soak -- --ignored"]
fn long_random_soak() {
    for fat in [12, 16, 32] {
        for seed in 1..=8u64 {
            run(fat, seed.wrapping_mul(0x9E37_79B9_7F4A_7C15), 6000);
        }
    }
}
