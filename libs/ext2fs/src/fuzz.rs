//! Byte-script fuzzing of the driver, in two modes.
//!
//! [`run`] reads its input as a script. The first byte picks the mode and the
//! block size; the rest is the script. The same function is the libFuzzer
//! target (`fuzz/fuzz_targets/ext2fs.rs`) and the body of the seeded tests, so
//! a crash found by one replays under the other.
//!
//! **Model mode** (low bit clear). The script is a list of four-byte
//! operations (mkdir, create, write, truncate, unlink, rmdir, rename, read)
//! applied to a freshly formatted volume and to a plain `BTreeMap` model of
//! the tree. Every result must agree with the model (success versus failure,
//! file contents), and at the end the whole tree must match the model, the
//! image must pass [`fsck`], and it must still match after a remount. With bit
//! 2 of the first byte set the volume is mounted through a tiny write-back
//! cache (`cache/`), so the same scripts drive eviction and writeback.
//!
//! **Corruption mode** (low bit set). A small populated volume has bytes of
//! its metadata overwritten as the script says, then is mounted and exercised
//! (listing, reading, creating, writing, renaming, removing). Nothing may
//! panic or loop, whatever the bytes. No consistency is promised for a volume
//! that was corrupted, only safety.

use std::collections::BTreeMap;
use std::string::{String, ToString};
use std::vec::Vec;

use crate::check::fsck;
use crate::memio::MemIo;
use crate::{Ext2, Ext2Error, FileKind, Geometry, Owner};

const VOLUME_BYTES: u64 = 2 * 1024 * 1024;
const UUID: [u8; 16] = *b"ext2fs-fuzz-uuid";
/// Metadata lives in the first blocks of a tiny volume; corrupt only there.
const CORRUPT_SPAN: usize = 48 * 1024;
/// Most entries the corruption walk visits, so a cyclic image ends it.
const WALK_BUDGET: usize = 400;

fn clock() -> i64 {
    1_700_000_000
}

enum Node {
    Dir,
    File(Vec<u8>),
}

type Model = BTreeMap<String, Node>;

/// Run one fuzz script; panics on any violated invariant.
pub fn run(data: &[u8]) {
    let Some((&head, script)) = data.split_first() else {
        return;
    };
    let block_size = if head & 2 == 0 { 1024 } else { 4096 };
    let io = MemIo::new(VOLUME_BYTES as usize);
    let geometry = Geometry {
        block_size,
        blocks_count: (VOLUME_BYTES / u64::from(block_size)) as u32,
        bytes_per_inode: 16 * 1024,
    };
    crate::format(&io, geometry, "fuzz", UUID, clock()).expect("format");
    if head & 1 == 0 {
        model_mode(&io, script, head & 4 != 0);
    } else {
        corruption_mode(&io, script);
    }
}

fn open(io: &MemIo) -> Ext2 {
    Ext2::open(alloc::boxed::Box::new(io.clone()), clock).expect("a formatted volume opens")
}

/// Mount through a cache of eight blocks: small enough that every script
/// evicts, writes back under pressure and reads ahead.
fn open_cached(io: &MemIo) -> Ext2 {
    let config = crate::CacheConfig::heap(8);
    Ext2::open_cached(alloc::boxed::Box::new(io.clone()), clock, config)
        .expect("a formatted volume opens")
}

/// The path an operation names: directory `d0`/`d1` (or the root) and file `fN`.
fn path_of(dir: u8, file: Option<u8>) -> String {
    let mut path = match dir % 3 {
        0 => String::new(),
        n => std::format!("/d{}", n - 1),
    };
    if let Some(file) = file {
        path.push_str(&std::format!("/f{}", file % 4));
    }
    if path.is_empty() {
        path.push('/');
    }
    path
}

fn model_mode(io: &MemIo, script: &[u8], cached: bool) {
    let fs = if cached { open_cached(io) } else { open(io) };
    let mut model = Model::new();
    for op in script.as_chunks::<4>().0.iter().take(300) {
        if !step(&fs, &mut model, op) {
            break; // out of space: the model and the volume may part ways
        }
    }
    compare_tree(&fs, &model);
    fs.flush().expect("flush");
    drop(fs);
    let problems = fsck(&io.snapshot());
    assert!(problems.is_empty(), "fsck: {problems:?}");
    compare_tree(&open(io), &model);
}

/// Apply one operation to the volume and the model; `false` on `NoSpace`.
fn step(fs: &Ext2, model: &mut Model, op: &[u8]) -> bool {
    let (kind, a, b, c) = (op[0] % 8, op[1], op[2], op[3]);
    let tag = std::format!("op {op:?}");
    let dir = path_of(a, None);
    let file = path_of(a, Some(b));
    let parent_ok = dir == "/" || matches!(model.get(&dir), Some(Node::Dir));
    let result = match kind {
        0 => {
            let path = std::format!("/d{}", b % 2);
            let ok = !model.contains_key(&path);
            check(
                &tag,
                fs.mkdir(&path, 0o755, Owner::ROOT).map(drop),
                ok,
                || {
                    model.insert(path, Node::Dir);
                },
            )
        }
        1 => {
            let ok = parent_ok && !model.contains_key(&file);
            check(
                &tag,
                fs.create(&file, 0o644, Owner::ROOT).map(drop),
                ok,
                || {
                    model.insert(file, Node::File(Vec::new()));
                },
            )
        }
        2 => write_op(fs, model, &file, usize::from(b) * 131, usize::from(c) * 97),
        3 => {
            let size = usize::from(c) * 131;
            let ok = matches!(model.get(&file), Some(Node::File(_)));
            check(&tag, fs.truncate(&file, size as u64), ok, || {
                if let Some(Node::File(bytes)) = model.get_mut(&file) {
                    bytes.resize(size, 0);
                }
            })
        }
        4 => {
            let ok = matches!(model.get(&file), Some(Node::File(_)));
            check(&tag, fs.unlink(&file), ok, || {
                model.remove(&file);
            })
        }
        5 => {
            let path = std::format!("/d{}", b % 2);
            let ok = matches!(model.get(&path), Some(Node::Dir))
                && !model
                    .keys()
                    .any(|k| k.starts_with(&std::format!("{path}/")));
            check(&tag, fs.rmdir(&path), ok, || {
                model.remove(&path);
            })
        }
        6 => rename_op(fs, model, &file, &path_of(c, Some(b))),
        _ => {
            let want = match model.get(&file) {
                Some(Node::File(bytes)) => Some(bytes.clone()),
                _ => None,
            };
            match (fs.read_file(&file), want) {
                (Ok(got), Some(want)) => assert_eq!(got, want, "read {file}"),
                (Err(_), None) => {}
                (got, want) => panic!(
                    "read {file}: {:?} vs model {:?}",
                    got.map(|v| v.len()),
                    want
                ),
            }
            Ok(())
        }
    };
    !matches!(result, Err(Ext2Error::NoSpace))
}

/// Compare a result with what the model predicts, applying `commit` on success.
fn check(
    tag: &str,
    result: Result<(), Ext2Error>,
    expect_ok: bool,
    commit: impl FnOnce(),
) -> Result<(), Ext2Error> {
    match (&result, expect_ok) {
        (Ok(()), true) => commit(),
        (Err(Ext2Error::NoSpace), _) | (Err(_), false) => {}
        (Ok(()), false) => panic!("{tag}: the volume accepted what the model refuses"),
        (Err(error), true) => panic!("{tag}: the volume refused what the model allows: {error:?}"),
    }
    result
}

fn write_op(
    fs: &Ext2,
    model: &mut Model,
    file: &str,
    offset: usize,
    len: usize,
) -> Result<(), Ext2Error> {
    let payload: Vec<u8> = (0..len).map(|i| (i * 7 + offset) as u8 | 1).collect();
    let Some(Node::File(bytes)) = model.get_mut(file) else {
        return match fs.write(file, offset as u64, &payload) {
            Ok(_) => panic!("write to a missing file succeeded"),
            Err(error) => Err(error),
        };
    };
    let written = fs.write(file, offset as u64, &payload)?;
    if written > 0 {
        if bytes.len() < offset + written {
            bytes.resize(offset + written, 0);
        }
        bytes[offset..offset + written].copy_from_slice(&payload[..written]);
    }
    if written < len {
        return Err(Ext2Error::NoSpace);
    }
    Ok(())
}

fn rename_op(fs: &Ext2, model: &mut Model, from: &str, to: &str) -> Result<(), Ext2Error> {
    let to_dir = to.rsplit_once('/').map_or("", |(dir, _)| dir);
    let source_is_file = matches!(model.get(from), Some(Node::File(_)));
    let dest_ok = to_dir.is_empty() || matches!(model.get(to_dir), Some(Node::Dir));
    let dest_free_or_file = !matches!(model.get(to), Some(Node::Dir));
    // Renaming a path onto itself is a no-op that succeeds even when it names
    // nothing (the driver short-circuits before it looks the path up).
    let expect_ok = from == to || (source_is_file && dest_ok && dest_free_or_file);
    check(
        &std::format!("rename {from} {to}"),
        fs.rename(from, to),
        expect_ok,
        || {
            if from != to {
                let node = model.remove(from).expect("source");
                model.insert(to.to_string(), node);
            }
        },
    )
}

/// Every model node exists with the same contents, and nothing else does.
fn compare_tree(fs: &Ext2, model: &Model) {
    let mut found = Model::new();
    walk(fs, "", &mut found);
    assert_eq!(found.len(), model.len(), "the tree has a different size");
    for (path, node) in model {
        match (node, found.get(path)) {
            (Node::Dir, Some(Node::Dir)) => {}
            (Node::File(want), Some(Node::File(got))) => {
                assert_eq!(got, want, "contents of {path}")
            }
            _ => panic!("{path} differs from the model"),
        }
    }
}

fn walk(fs: &Ext2, dir: &str, out: &mut Model) {
    let listing = fs
        .readdir(if dir.is_empty() { "/" } else { dir })
        .expect("readdir");
    for entry in listing {
        if dir.is_empty() && entry.name == "lost+found" {
            continue;
        }
        let path = std::format!("{dir}/{}", entry.name);
        match entry.kind {
            FileKind::Dir => {
                out.insert(path.clone(), Node::Dir);
                walk(fs, &path, out);
            }
            FileKind::File => {
                out.insert(path.clone(), Node::File(fs.read_file(&path).expect("read")));
            }
        }
    }
}

/// Overwrite metadata bytes as the script says, then use the volume.
fn corruption_mode(io: &MemIo, script: &[u8]) {
    {
        let fs = open(io);
        for dir in ["/d0", "/d0/sub", "/d1"] {
            fs.mkdir(dir, 0o755, Owner::ROOT).expect("mkdir");
        }
        for (path, size) in [("/a", 100usize), ("/d0/b", 5000), ("/d0/sub/c", 40_000)] {
            fs.write_file(path, &std::vec![7u8; size], 0o644, 0, 0, 5)
                .expect("seed");
        }
        fs.flush().expect("flush");
    }
    io.with_bytes(|image| {
        for edit in script.as_chunks::<3>().0.iter().take(64) {
            let at = (usize::from(edit[0]) << 8 | usize::from(edit[1])) % CORRUPT_SPAN;
            image[at] = edit[2];
        }
    });
    let Ok(fs) = Ext2::open(alloc::boxed::Box::new(io.clone()), clock) else {
        return; // refused: the right answer for most corruptions
    };
    let mut budget = WALK_BUDGET;
    exercise(&fs, "/", 0, &mut budget);
    let _ = fs.statfs();
    let _ = fs.mkdir("/new", 0o755, Owner::ROOT);
    let _ = fs.write_file("/new/x", &[1u8; 3000], 0o600, 1, 2, 9);
    let _ = fs.rename("/a", "/new/a");
    let _ = fs.truncate("/new/x", 10);
    let _ = fs.reclaim_orphans(".unlinked-");
    let _ = fs.remove_tree("/new");
    let _ = fs.remove_tree("/d0");
    let _ = fs.flush();
}

fn exercise(fs: &Ext2, dir: &str, depth: usize, budget: &mut usize) {
    let Ok(entries) = fs.readdir(dir) else { return };
    for entry in entries {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let path = if dir == "/" {
            std::format!("/{}", entry.name)
        } else {
            std::format!("{dir}/{}", entry.name)
        };
        let _ = fs.lookup(&path);
        match entry.kind {
            FileKind::File => {
                let mut buf = [0u8; 600];
                let _ = fs.read(&path, 0, &mut buf);
                let _ = fs.read(&path, 70_000, &mut buf);
                let _ = fs.link_count(&path);
                let _ = fs.mapped_block(&path, 0);
            }
            FileKind::Dir if depth < 6 => exercise(fs, &path, depth + 1, budget),
            FileKind::Dir => {}
        }
    }
}
