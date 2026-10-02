//! A random file-tree workload for the cache tests: the same seeded operations
//! applied to several volumes at once, with a model of what each file holds.
//!
//! Every file's bytes are one *tag* value (a file keeps its tag through
//! rewrites, appends and renames), so a crashed image can be searched for
//! bytes that leaked from one file into another.

use std::collections::BTreeMap;
use std::format;

use fuzzkit::Rng;

use super::*;
use crate::{Ext2Error, FileKind, Owner};

/// How many seeds a heavy test runs: `default`, or `FUZZ_CASES` for a soak.
/// (`for_seeds` runs `FUZZ_CASES` cases; the tests stop theirs at this.)
pub fn cases(default: usize) -> usize {
    std::env::var("FUZZ_CASES")
        .ok()
        .and_then(|cases| cases.parse().ok())
        .unwrap_or(default)
}

/// Directories the workload may create, parents before children.
const DIRS: [&str; 6] = ["/a", "/b", "/c", "/a/x", "/a/y", "/b/z"];

/// What the volumes should hold.
#[derive(Default)]
pub struct Model {
    pub dirs: Vec<String>,
    /// Path -> (tag, contents).
    pub files: BTreeMap<String, (u8, Vec<u8>)>,
    next_tag: u8,
}

impl Model {
    fn tag(&mut self) -> u8 {
        self.next_tag = self.next_tag % 250 + 1;
        self.next_tag
    }
}

/// A size that is usually small but reaches the single- and (with 1 KiB
/// blocks) double-indirect ranges.
fn size(rng: &mut Rng) -> usize {
    match rng.below(10) {
        0..=5 => rng.below(6_000) as usize,
        6..=8 => rng.below(70_000) as usize,
        _ => 270_000 + rng.below(30_000) as usize,
    }
}

/// Apply one random operation to every volume in `volumes`; they must all
/// answer alike. Returns `false` once the volumes are full.
pub fn step(volumes: &[&Ext2], model: &mut Model, rng: &mut Rng) -> bool {
    let all = |op: &dyn Fn(&Ext2) -> Result<(), Ext2Error>| -> Result<(), Ext2Error> {
        let results: Vec<_> = volumes.iter().map(|fs| op(fs)).collect();
        assert!(
            results.windows(2).all(|pair| pair[0] == pair[1]),
            "volumes disagree: {results:?}"
        );
        results[0]
    };
    let dir = if model.dirs.is_empty() || rng.one_in(6) {
        String::from("")
    } else {
        model.dirs[rng.below(model.dirs.len() as u64) as usize].clone()
    };
    let file = format!("{dir}/f{}", rng.below(6));
    let result = match rng.below(12) {
        0 => {
            let path = DIRS[rng.below(DIRS.len() as u64) as usize];
            let parent_ok = path
                .rfind('/')
                .is_some_and(|at| at == 0 || model.dirs.iter().any(|d| d == &path[..at]));
            if !parent_ok || model.dirs.iter().any(|d| d == path) {
                return true;
            }
            let result = all(&|fs| fs.mkdir(path, 0o755, Owner::ROOT).map(drop));
            if result.is_ok() {
                model.dirs.push(String::from(path));
            }
            result
        }
        1..=4 => {
            let tag = match model.files.get(&file) {
                Some((tag, _)) => *tag,
                None => model.tag(),
            };
            let data = std::vec![tag; size(rng)];
            let result = all(&|fs| fs.write_file(&file, &data, 0o644, 0, 0, 7).map(drop));
            if result.is_ok() {
                model.files.insert(file, (tag, data));
            }
            result
        }
        5..=6 => {
            let Some((tag, data)) = model.files.get_mut(&file) else {
                return true;
            };
            let offset = rng.below(data.len() as u64 + 3_000) as usize;
            let patch = std::vec![*tag; rng.below(9_000) as usize];
            let result = all(&|fs| {
                let written = fs.write(&file, offset as u64, &patch)?;
                assert_eq!(written, patch.len());
                Ok(())
            });
            if result.is_ok() {
                if data.len() < offset + patch.len() {
                    data.resize(offset + patch.len(), 0);
                }
                data[offset..offset + patch.len()].copy_from_slice(&patch);
            }
            result
        }
        7 => {
            let Some((_, data)) = model.files.get_mut(&file) else {
                return true;
            };
            let to = rng.below(data.len() as u64 * 2 + 1) as usize;
            let result = all(&|fs| fs.truncate(&file, to as u64));
            if result.is_ok() {
                data.resize(to, 0);
            }
            result
        }
        8..=9 => {
            if !model.files.contains_key(&file) {
                return true;
            }
            let result = all(&|fs| fs.unlink(&file));
            if result.is_ok() {
                model.files.remove(&file);
            }
            result
        }
        10 => {
            let to = format!("{}/f{}", dir, rng.below(6));
            if !model.files.contains_key(&file) || to == file {
                return true;
            }
            let result = all(&|fs| fs.rename(&file, &to));
            if result.is_ok() {
                let moved = model.files.remove(&file).expect("in the model");
                model.files.insert(to, moved);
            }
            result
        }
        _ => {
            // rmdir of an empty leaf directory.
            let Some(index) = model.dirs.iter().position(|d| {
                !model.files.keys().any(|f| f.starts_with(&format!("{d}/")))
                    && !model.dirs.iter().any(|c| c.starts_with(&format!("{d}/")))
            }) else {
                return true;
            };
            let path = model.dirs[index].clone();
            let result = all(&|fs| fs.rmdir(&path));
            if result.is_ok() {
                model.dirs.remove(index);
            }
            result
        }
    };
    match result {
        Ok(()) => true,
        Err(Ext2Error::NoSpace) => false,
        Err(error) => panic!("unexpected {error:?}"),
    }
}

/// Every file of the model reads back exactly from `fs`.
pub fn verify(fs: &Ext2, model: &Model) {
    for (path, (_, data)) in &model.files {
        let mut back = std::vec![0u8; data.len() + 1];
        let read = fs
            .read(path, 0, &mut back)
            .unwrap_or_else(|e| panic!("{path}: {e:?}"));
        assert_eq!(read, data.len(), "{path}: length");
        assert!(back[..read] == data[..], "{path}: contents differ");
    }
}

/// Visit every regular file reachable from the root of `fs` (errors on a
/// damaged image end that branch), with its contents.
pub fn walk(fs: &Ext2, visit: &mut dyn FnMut(&str, &[u8])) {
    let mut queue = std::vec![String::from("")];
    let mut budget = 2_000usize;
    while let Some(dir) = queue.pop() {
        let Ok(entries) = fs.readdir(if dir.is_empty() { "/" } else { &dir }) else {
            continue;
        };
        for entry in entries {
            budget = match budget.checked_sub(1) {
                Some(left) => left,
                None => return, // a cyclic image
            };
            if entry.name == "." || entry.name == ".." {
                continue;
            }
            let path = format!("{dir}/{}", entry.name);
            match entry.kind {
                FileKind::Dir => queue.push(path),
                FileKind::File => {
                    let Ok(meta) = fs.lookup(&path) else { continue };
                    let mut data = std::vec![0u8; meta.size.min(1 << 20) as usize];
                    if let Ok(read) = fs.read(&path, 0, &mut data) {
                        visit(&path, &data[..read]);
                    }
                }
            }
        }
    }
}
