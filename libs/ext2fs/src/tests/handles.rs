//! File handles (`handle.rs`) and run reads (`read_run.rs`, `cache/range.rs`):
//! a handle reads and writes exactly what the path does, goes stale when its
//! inode is freed or reused, and a read of any shape returns the model's bytes
//! whether the blocks are cached, dirty, uncached, contiguous or fragmented.

use super::*;
use crate::{Ext2Error, FileHandle, Owner};

/// Deterministic bytes for file `seed` at `offset`.
fn byte(seed: u64, offset: u64) -> u8 {
    let mixed = offset.wrapping_add(seed.wrapping_mul(0xD1B5_4A32_D192_ED03));
    (mixed.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as u8
}

fn bytes(seed: u64, offset: u64, len: usize) -> Vec<u8> {
    (0..len as u64).map(|i| byte(seed, offset + i)).collect()
}

/// A small xorshift, so the shapes are reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }
}

#[test]
fn handle_reads_and_writes_like_the_path() {
    for block_size in BLOCK_SIZES {
        let (io, fs) = fresh(8 << 20, block_size);
        fs.create("/f", 0o644, Owner::ROOT).unwrap();
        let handle = fs.open_file("/f").unwrap();
        let data = bytes(1, 0, 300_000);
        assert_eq!(fs.write_handle(handle, 0, &data).unwrap(), data.len());
        let mut back = std::vec![0u8; data.len()];
        assert_eq!(fs.read("/f", 0, &mut back).unwrap(), data.len());
        assert_eq!(back, data);
        // Writes through the path show through the handle.
        fs.write("/f", 5000, b"hello").unwrap();
        let mut five = [0u8; 5];
        fs.read_handle(handle, 5000, &mut five).unwrap();
        assert_eq!(&five, b"hello");
        assert_eq!(fs.handle_meta(handle).unwrap().size, data.len() as u64);
        // A rename does not change the inode.
        fs.rename("/f", "/g").unwrap();
        assert_eq!(fs.read_handle(handle, 5000, &mut five).unwrap(), 5);
        assert_eq!(fs.open_file("/"), Err(Ext2Error::IsDir));
        drop(fs);
        assert_clean(&io);
    }
}

#[test]
fn a_handle_goes_stale_when_its_inode_is_freed_or_reused() {
    let (io, fs) = fresh(4 << 20, 1024);
    fs.create("/old", 0o600, Owner { uid: 7, gid: 7 }).unwrap();
    fs.write("/old", 0, b"secret").unwrap();
    let old = fs.open_file("/old").unwrap();
    fs.unlink("/old").unwrap();
    let mut buf = [0u8; 6];
    assert_eq!(fs.read_handle(old, 0, &mut buf), Err(Ext2Error::NotFound));
    assert_eq!(fs.write_handle(old, 0, b"x"), Err(Ext2Error::NotFound));
    // The next create takes the same inode (the lowest free one) with the
    // next generation: the old handle must not read the new file.
    fs.create("/new", 0o644, Owner::ROOT).unwrap();
    fs.write("/new", 0, b"public").unwrap();
    let new = fs.open_file("/new").unwrap();
    assert_eq!(new.ino(), old.ino(), "the inode was reused");
    assert_eq!(new.generation(), old.generation().wrapping_add(1));
    assert_eq!(fs.read_handle(old, 0, &mut buf), Err(Ext2Error::NotFound));
    assert_eq!(fs.read_handle(new, 0, &mut buf).unwrap(), 6);
    assert_eq!(&buf, b"public");
    // A forged handle is refused, not misread.
    let forged = FileHandle::from_parts(new.ino(), new.generation() ^ 0x55);
    assert_eq!(
        fs.read_handle(forged, 0, &mut buf),
        Err(Ext2Error::NotFound)
    );
    let dir = fs.mkdir("/d", 0o755, Owner::ROOT).unwrap();
    let as_file = FileHandle::from_parts(dir.ino as u32, 1);
    assert_eq!(
        fs.read_handle(as_file, 0, &mut buf),
        Err(Ext2Error::NotFound)
    );
    assert!(fs
        .read_handle(FileHandle::from_parts(u32::MAX, 0), 0, &mut buf)
        .is_err());
    drop(fs);
    assert_clean(&io);
}

/// Two files written in alternating pieces (fragmented), one with holes, and
/// one contiguous; every read shape against the model, through `open`.
fn reads_match_the_model(fs: &Ext2, block_size: u32, seed: u64) {
    let block = block_size as u64;
    let mut rng = Rng(seed | 1);
    let mut models: Vec<(String, Vec<u8>)> = Vec::new();
    for name in ["/a", "/b", "/sparse", "/flat"] {
        fs.create(name, 0o644, Owner::ROOT).unwrap();
        models.push((String::from(name), Vec::new()));
    }
    // `/a` and `/b` interleave, a few blocks at a time: runs break often.
    for step in 0..40u64 {
        for (index, name) in ["/a", "/b"].iter().enumerate() {
            let model = &mut models[index].1;
            let len = (1 + rng.below(5)) as usize * block as usize + rng.below(block) as usize;
            let data = bytes(index as u64 + step, model.len() as u64, len);
            fs.write(name, model.len() as u64, &data).unwrap();
            model.extend_from_slice(&data);
        }
    }
    // `/sparse`: islands far apart, so the block map has holes at every level.
    let sparse = &mut models[2].1;
    for island in 0..6u64 {
        let at = island * (block * 300 + 17);
        let data = bytes(99, at, 3 * block as usize + 5);
        fs.write("/sparse", at, &data).unwrap();
        if sparse.len() < at as usize {
            sparse.resize(at as usize, 0);
        }
        sparse.truncate(at as usize);
        sparse.extend_from_slice(&data);
    }
    let flat = bytes(7, 0, 9_000_000);
    fs.write("/flat", 0, &flat).unwrap();
    models[3].1 = flat;

    for round in 0..400u64 {
        let (name, model) = &models[(round % 4) as usize];
        let size = model.len() as u64;
        let offset = rng.below(size + block);
        let len = match round % 5 {
            0 => rng.below(block) as usize + 1,
            1 => rng.below(8 * block) as usize,
            2 => (rng.below(80) * block) as usize,
            _ => rng.below(1_200_000) as usize + 1,
        };
        let mut got = std::vec![0xEEu8; len];
        let read = fs.read(name, offset, &mut got).unwrap();
        let want_len = (size.saturating_sub(offset) as usize).min(len);
        assert_eq!(read, want_len, "{name} at {offset} len {len}");
        let start = offset.min(size) as usize;
        assert!(
            got[..read] == model[start..start + read],
            "{name}: bytes differ in a read at {offset} of {len} (block {block_size})"
        );
    }
}

#[test]
fn run_reads_match_the_model_uncached() {
    for (seed, block_size) in BLOCK_SIZES.iter().enumerate() {
        let (io, fs) = fresh(32 << 20, *block_size);
        reads_match_the_model(&fs, *block_size, seed as u64 + 11);
        drop(fs);
        assert_clean(&io);
    }
}

#[test]
fn run_reads_match_the_model_through_small_and_large_caches() {
    for (seed, block_size) in BLOCK_SIZES.iter().enumerate() {
        // 24 blocks: everything evicts and bypasses; 4096: most stays dirty.
        for pages in [24usize, 4096] {
            let io = formatted(32 << 20, *block_size);
            let fs = open_cached(&io, pages);
            reads_match_the_model(&fs, *block_size, seed as u64 * 31 + pages as u64);
            fs.flush().unwrap();
            drop(fs);
            assert_clean(&io);
        }
    }
}

/// Dirty cached blocks in the middle of an uncached run: a bypass read must
/// take them from the cache, never the stale disk copy.
#[test]
fn a_bypass_read_keeps_dirty_blocks_in_the_middle() {
    let io = formatted(48 << 20, 4096);
    let fs = open_cached(&io, 512);
    fs.create("/big", 0o644, Owner::ROOT).unwrap();
    let mut model = bytes(3, 0, 12 << 20);
    fs.write("/big", 0, &model).unwrap();
    fs.flush().unwrap();
    // Push the file out of the cache, then dirty a few blocks of it.
    fs.create("/other", 0o644, Owner::ROOT).unwrap();
    fs.write("/other", 0, &bytes(4, 0, 3 << 20)).unwrap();
    fs.flush().unwrap();
    for at in [100_000u64, 1_000_003, 2_500_000] {
        let patch = bytes(5, at, 9000);
        fs.write("/big", at, &patch).unwrap();
        model[at as usize..at as usize + patch.len()].copy_from_slice(&patch);
    }
    let mut got = std::vec![0u8; model.len()];
    assert_eq!(fs.read("/big", 0, &mut got).unwrap(), model.len());
    assert!(got == model, "a dirty block was read from the disk");
    let stats = fs.cache_stats().unwrap();
    assert!(stats.bypassed > 0, "{stats:?}");
    fs.flush().unwrap();
    drop(fs);
    assert_clean(&io);
}

/// A file deep in the double-indirect range (1 KiB blocks: 12 + 256 direct
/// and single-indirect blocks) reads back across every table boundary.
#[test]
fn run_reads_cross_indirect_boundaries() {
    let (io, fs) = fresh(16 << 20, 1024);
    fs.create("/deep", 0o644, Owner::ROOT).unwrap();
    let data = bytes(8, 0, (12 + 256 + 3 * 256 + 7) * 1024 + 300);
    fs.write("/deep", 0, &data).unwrap();
    for (offset, len) in [
        (0usize, data.len()),
        (11 * 1024 + 1000, 3000),
        (267 * 1024, 2048),
    ] {
        let mut got = std::vec![0u8; len];
        assert_eq!(fs.read("/deep", offset as u64, &mut got).unwrap(), len);
        assert!(got[..] == data[offset..offset + len], "at {offset}");
    }
    drop(fs);
    assert_clean(&io);
}
