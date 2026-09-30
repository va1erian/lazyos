//! FAT soak (issue #414): thousands of random lookups, reads and listings over
//! a wide, deep synthetic tree with long names, checked against what was
//! written, with the heap watched for growth and the path cache for its cap.

use super::fat_image::*;
use super::*;
use crate::fs::vfs::Filesystem;
use alloc::vec::Vec;

const DEPTH: usize = 4;
const FANOUT: usize = 3;
const LEAF_FILES: usize = 3;
const WIDE_FILES: usize = 100;
/// Slack for allocator noise; a leak per operation would blow far past this.
const HEAP_SLACK: usize = 32 * 1024;

/// xorshift: deterministic pseudo-random choices.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

/// What the builder wrote: every file with its bytes, every directory with
/// its entry count.
#[derive(Default)]
struct Tree {
    files: Vec<(String, Vec<u8>)>,
    dirs: Vec<(String, usize)>,
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        String::from(name)
    } else {
        format!("{parent}/{name}")
    }
}

/// Store `slots` (dot slots first) for the directory whose first cluster is
/// `first`, extending it with more clusters when it does not fit in one.
fn store_growing(img: &mut FatImage, first: u16, slots: &[Slot]) {
    let mut clusters = alloc::vec![first];
    while clusters.len() * SLOTS_PER_CLUSTER < slots.len() {
        clusters.push(img.alloc());
    }
    img.store_dir_in(&clusters, slots);
}

fn add_file(
    img: &mut FatImage,
    rng: &mut Rng,
    tree: &mut Tree,
    dir: &str,
    name: String,
    short: &str,
) -> Vec<Slot> {
    let len = 1 + rng.below(1200);
    let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
    let cluster = img.store_file(&bytes);
    let slots = if name.chars().all(|c| c.is_ascii_uppercase() || c == '.') && name.len() <= 12 {
        alloc::vec![short_slot(&s83(&name), ATTR_FILE, cluster, len as u32)]
    } else {
        long_entry(&name, &s83(short), ATTR_FILE, cluster, len as u32)
    };
    tree.files.push((join(dir, &name), bytes));
    slots
}

/// The entries of directory `path` (dot slots excluded), built recursively.
fn fill(
    img: &mut FatImage,
    rng: &mut Rng,
    tree: &mut Tree,
    path: &str,
    own: u16,
    depth: usize,
) -> Vec<Slot> {
    let mut slots = Vec::new();
    let mut count = 0;
    if depth < DEPTH {
        for i in 0..FANOUT {
            let name = format!("Directory level {depth} number {i}");
            let child_path = join(path, &name);
            let child = img.alloc();
            let body = fill(img, rng, tree, &child_path, child, depth + 1);
            let mut all = dot_slots(child, own).to_vec();
            all.extend(body);
            store_growing(img, child, &all);
            slots.extend(long_entry(
                &name,
                &s83(&format!("DIR{depth}{i}~1")),
                ATTR_DIR,
                child,
                0,
            ));
            count += 1;
        }
    }
    let files = if depth == DEPTH { LEAF_FILES } else { 1 };
    for i in 0..files {
        let (name, short) = if i == 0 {
            (String::from("PLAIN.TXT"), String::new())
        } else {
            (
                format!("leaf file {i} of depth {depth}.bin"),
                format!("LEAF{i}~1.BIN"),
            )
        };
        slots.extend(add_file(img, rng, tree, path, name, &short));
        count += 1;
    }
    tree.dirs.push((String::from(path), count));
    slots
}

fn build_tree(rng: &mut Rng) -> (FatImage, Tree) {
    let mut img = FatImage::new(1600, false);
    let mut tree = Tree::default();
    let mut root = fill(&mut img, rng, &mut tree, "", 0, 0);

    // One directory that is wide rather than deep.
    let wide = img.alloc();
    let mut slots = dot_slots(wide, 0).to_vec();
    for i in 0..WIDE_FILES {
        let name = format!("wide file number {i:03}.txt");
        slots.extend(add_file(
            &mut img,
            rng,
            &mut tree,
            "wide dir",
            name,
            &format!("WIDE{i:03}~1.TXT"),
        ));
    }
    store_growing(&mut img, wide, &slots);
    tree.dirs.push((String::from("wide dir"), WIDE_FILES));
    root.extend(long_entry("wide dir", &s83("WIDEDI~1"), ATTR_DIR, wide, 0));
    if let Some(entry) = tree.dirs.iter_mut().find(|(path, _)| path.is_empty()) {
        entry.1 += 1;
    }
    img.set_root(&root);
    (img, tree)
}

/// Flip the case of a random subset of `path`'s ASCII letters.
fn mangle(rng: &mut Rng, path: &str) -> String {
    path.chars()
        .map(|ch| match rng.below(3) {
            0 => ch.to_ascii_uppercase(),
            1 => ch.to_ascii_lowercase(),
            _ => ch,
        })
        .collect()
}

fn one_round(fs: &dyn Filesystem, tree: &Tree, rng: &mut Rng) -> Result<(), String> {
    match rng.below(20) {
        0..=11 => {
            let (path, bytes) = &tree.files[rng.below(tree.files.len())];
            let spelled = mangle(rng, path);
            let meta = fs
                .lookup(&spelled)
                .map_err(|e| format!("lookup {spelled}: {}", fs_error(e)))?;
            check!(
                meta.size == bytes.len() as u64,
                "{spelled}: size {}",
                meta.size
            );
            let offset = rng.below(bytes.len());
            let mut buf = alloc::vec![0u8; 1 + rng.below(700)];
            let got = fs
                .read(&spelled, offset as u64, &mut buf)
                .map_err(fs_error)?;
            let end = (offset + buf.len()).min(bytes.len());
            check!(
                got == end - offset && buf[..got] == bytes[offset..end],
                "{spelled}: read {offset}+{} differs",
                buf.len()
            );
        }
        12..=14 => {
            let (path, count) = &tree.dirs[rng.below(tree.dirs.len())];
            let listed = fs.readdir(&mangle(rng, path)).map_err(fs_error)?;
            check!(
                listed.len() == *count,
                "{path:?} lists {} entries, wrote {count}",
                listed.len()
            );
        }
        15..=16 => {
            let (path, _) = &tree.files[rng.below(tree.files.len())];
            check!(
                fs.lookup(&format!("{path}x")).err() == Some(FsError::NotFound),
                "{path}x exists"
            );
            check!(
                fs.lookup(&format!("{path}/x")).err() == Some(FsError::NotDir),
                "{path}/x is not NotDir"
            );
        }
        _ => {
            let (path, _) = &tree.dirs[rng.below(tree.dirs.len())];
            check!(
                fs.read(path, 0, &mut [0u8; 4]).err() == Some(FsError::IsDir),
                "read of {path:?}"
            );
        }
    }
    Ok(())
}

fn heap_used() -> usize {
    crate::mem::slab::stats().live_bytes + crate::mem::heap_stats().used
}

/// Random lookups, reads and listings over the tree; heap use holds steady
/// and the path cache never outgrows its cap.
pub fn fat_soak_random_tree() -> Result<(), String> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (img, tree) = build_tree(&mut rng);
    let fs = img.mount("test-fat-soak-tree");
    drop(img);
    check!(
        tree.files.len() > 300 && tree.dirs.len() > 100,
        "the tree is too small to soak"
    );

    // Warm up: caches fill, and one-off allocations settle.
    for _ in 0..2000 {
        one_round(&fs, &tree, &mut rng)?;
    }
    let before = heap_used();
    for round in 0..8000 {
        one_round(&fs, &tree, &mut rng).map_err(|e| format!("round {round}: {e}"))?;
        if round % 1000 == 0 {
            let cached = fs.cached_paths();
            check!(
                cached > 0 && cached <= 128,
                "the path cache holds {cached} paths"
            );
        }
    }
    let after = heap_used();
    check!(
        after <= before + HEAP_SLACK,
        "heap grew from {before} to {after} bytes over the soak"
    );
    Ok(())
}
