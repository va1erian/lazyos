//! `fsync` of one file and the ordering points of a rename write only what
//! they need (`cache/targeted.rs`): unrelated dirty blocks stay dirty, and
//! the crash semantics of `docs/architecture/block-cache.md` still hold at
//! every write.

use std::sync::Arc;

use fuzzkit::for_seeds;

use super::cache_crash::{holds, image_at, judge, Recorder};
use super::workload::{self, Model};
use super::*;
use crate::{CacheConfig, FileHandle};

fn recorded(block_size: u32, blocks: usize) -> (MemIo, Recorder, Ext2) {
    let disk = formatted(8 << 20, block_size);
    let recorder = Recorder {
        disk: disk.clone(),
        block_size: block_size as usize,
        log: Arc::default(),
    };
    let fs = Ext2::open_cached(
        Box::new(recorder.clone()),
        clock,
        CacheConfig::heap(blocks),
    )
    .unwrap();
    (disk, recorder, fs)
}

/// A read-only mount of the bytes `disk` holds right now: what a power cut
/// at this moment would leave.
fn crashed(disk: &MemIo) -> Ext2 {
    let io = MemIo::from_bytes(disk.snapshot());
    io.set_writable(false);
    Ext2::open(Box::new(io), clock).expect("a crashed image still mounts")
}

fn read_all(fs: &Ext2, handle: FileHandle, len: usize) -> Vec<u8> {
    let mut back = std::vec![0u8; len + 1];
    let read = fs.read_handle(handle, 0, &mut back).unwrap();
    back.truncate(read);
    back
}

#[test]
fn sync_file_writes_that_file_and_not_the_others() {
    for block_size in BLOCK_SIZES {
        let (disk, _, fs) = recorded(block_size, 8192);
        fs.write_file("/small", &[1u8; 3_000], 0o644, 0, 0, 1)
            .unwrap();
        fs.flush().unwrap();
        // 600 KiB nobody asked to sync, then the small file grows.
        fs.write_file("/bulk", &[2u8; 600 * 1024], 0o644, 0, 0, 1)
            .unwrap();
        let small = fs.open_file("/small").unwrap();
        fs.write(&"/small", 3_000, &[3u8; 5_000]).unwrap();
        let dirty = fs.dirty_blocks();
        let written = fs.cache_stats().unwrap().written;

        fs.sync_file(small).unwrap();

        let wrote = fs.cache_stats().unwrap().written - written;
        let bulk_blocks = 600 * 1024 / block_size as u64;
        assert!(
            wrote < bulk_blocks / 4,
            "{block_size}: sync_file wrote {wrote} blocks, the other file has {bulk_blocks}"
        );
        assert!(
            fs.dirty_blocks() >= dirty / 2,
            "{block_size}: the other file is still dirty"
        );
        // What the disk holds now has the synced bytes.
        let image = crashed(&disk);
        let mut expect = std::vec![1u8; 3_000];
        expect.extend([3u8; 5_000]);
        assert!(read_all(&image, small, 9_000) == expect, "{block_size}");
        // The volume is not marked clean by a file sync.
        assert!(!image.was_clean_at_mount());
        drop(image);
        // A full flush then writes the rest.
        fs.flush().unwrap();
        assert_eq!(fs.dirty_blocks(), 0);
        assert!(fsck(&disk.snapshot()).is_empty());
    }
}

/// A file synced after other files were written and deleted around it: its
/// inode table block holds other inodes, whose fresh blocks must land first
/// (no file ever shows bytes that were not its own).
#[test]
fn a_neighbour_sharing_the_inode_block_never_shows_stale_bytes() {
    for block_size in BLOCK_SIZES {
        let (disk, recorder, fs) = recorded(block_size, 8192);
        // A freed block full of the old tag is reused by a neighbour.
        fs.write_file("/old", &[0xEEu8; 40_000], 0o644, 0, 0, 1)
            .unwrap();
        fs.flush().unwrap();
        fs.unlink("/old").unwrap();
        fs.flush().unwrap();
        let start = recorder.log.lock().unwrap().len();
        let base = disk.snapshot();
        fs.write_file("/neighbour", &[7u8; 40_000], 0o644, 0, 0, 1)
            .unwrap();
        fs.write_file("/synced", &[8u8; 100], 0o644, 0, 0, 1)
            .unwrap();
        let synced = fs.open_file("/synced").unwrap();
        fs.sync_file(synced).unwrap();
        fs.writeback().unwrap();
        drop(fs);
        let log = recorder.log.lock().unwrap()[start..].to_vec();
        for point in 0..=log.len() {
            let io = MemIo::from_bytes(image_at(&base, &log, point));
            io.set_writable(false);
            let image = Ext2::open(Box::new(io), clock).unwrap();
            workload::walk(&image, &mut |path, data| {
                assert!(
                    data.iter().all(|&byte| byte == 0 || byte == 7 || byte == 8),
                    "{block_size}: after write {point}, {path} shows the deleted file's bytes"
                );
            });
        }
    }
}

#[test]
fn sync_file_survives_a_crash_at_every_write() {
    let mut case = 0;
    for_seeds("sync_file_survives_a_crash_at_every_write", |_, rng| {
        case += 1;
        if case > workload::cases(40) {
            return;
        }
        let block_size = BLOCK_SIZES[rng.below(3) as usize];
        let disk = formatted(2 << 20, block_size);
        let base = disk.snapshot();
        let recorder = Recorder {
            disk: disk.clone(),
            block_size: block_size as usize,
            log: Arc::default(),
        };
        let blocks = [4, 16, 256][rng.below(3) as usize];
        let fs = Ext2::open_cached(
            Box::new(recorder.clone()),
            clock,
            CacheConfig::heap(blocks),
        )
        .unwrap();
        let mut model = Model::default();
        for _ in 0..rng.range(10, 60) {
            if !workload::step(&[&fs], &mut model, rng) {
                break;
            }
            if rng.below(3) != 0 || model.files.is_empty() {
                continue;
            }
            let index = rng.below(model.files.len() as u64) as usize;
            let (path, (_, data)) = model.files.iter().nth(index).unwrap();
            let handle = fs.open_file(path).unwrap();
            fs.sync_file(handle).unwrap();
            // Durable: the synced file's bytes are on the disk now.
            let image = crashed(&disk);
            let back = read_all(&image, handle, data.len());
            assert!(back == *data, "{path}: a synced file is on the disk");
        }
        drop(fs);
        let log = recorder.log.lock().unwrap().clone();
        for _ in 0..40 {
            let point = rng.below(log.len() as u64 + 1) as usize;
            judge(image_at(&base, &log, point), point);
        }
    });
}

/// Renames next to a large unsynced file: the rename still never loses the
/// file at any crash point, and does not write the large file out.
#[test]
fn a_rename_beside_unsynced_data_is_ordered_and_narrow() {
    for block_size in BLOCK_SIZES {
        let (disk, recorder, fs) = recorded(block_size, 8192);
        for dir in ["/a", "/b", "/x", "/y"] {
            fs.mkdir(dir, 0o755, crate::Owner::ROOT).unwrap();
        }
        for n in 0..60 {
            let name = std::format!("/a/padding-entry-with-a-long-name-{n:03}");
            fs.write_file(&name, &[5u8; 3], 0o644, 0, 0, 1).unwrap();
        }
        let (one, two, three) = ([1u8; 3000], [2u8; 5000], [3u8; 700]);
        fs.write_file("/a/one", &one, 0o644, 0, 0, 1).unwrap();
        fs.write_file("/b/two", &two, 0o644, 0, 0, 1).unwrap();
        fs.write_file("/x/three", &three, 0o644, 0, 0, 1).unwrap();
        fs.flush().unwrap();
        let base = disk.snapshot();
        let start = recorder.log.lock().unwrap().len();
        // Unrelated, unsynced, and bigger than everything the renames touch.
        fs.write_file("/bulk", &[9u8; 500 * 1024], 0o644, 0, 0, 1)
            .unwrap();
        let before = fs.cache_stats().unwrap().written;
        fs.rename("/a/one", "/b/two").unwrap();
        fs.rename("/b/two", "/a/moved-back").unwrap();
        fs.rename("/x", "/y/x").unwrap();
        let wrote = fs.cache_stats().unwrap().written - before;
        let bulk_blocks = 500 * 1024 / block_size as u64;
        assert!(
            wrote < bulk_blocks / 4,
            "{block_size}: the renames wrote {wrote} blocks, the bulk file has {bulk_blocks}"
        );
        assert!(fs.dirty_blocks() as u64 >= bulk_blocks / 2);
        fs.flush().unwrap();
        drop(fs);
        let log = recorder.log.lock().unwrap()[start..].to_vec();
        for point in 0..=log.len() {
            let image = image_at(&base, &log, point);
            judge(image.clone(), point);
            let io = MemIo::from_bytes(image);
            io.set_writable(false);
            let fs = Ext2::open(Box::new(io), clock).unwrap();
            let found = ["/a/one", "/b/two", "/a/moved-back"]
                .iter()
                .any(|path| holds(&fs, path, &one));
            assert!(
                found,
                "{block_size}: crash after write {point} lost the renamed file"
            );
            let moved = ["/x/three", "/y/x/three"]
                .iter()
                .any(|path| holds(&fs, path, &three));
            assert!(
                moved,
                "{block_size}: crash after write {point} lost the moved directory"
            );
        }
    }
}

/// The write-temp-then-rename pattern with the temp file never synced: the
/// rename writes the file's inode, so the file's fresh blocks must land
/// first, or a crash shows the bytes of the file that owned them before.
#[test]
fn renaming_an_unsynced_file_never_exposes_stale_bytes() {
    for block_size in BLOCK_SIZES {
        let (disk, recorder, fs) = recorded(block_size, 8192);
        fs.write_file("/old", &[0xEEu8; 60_000], 0o644, 0, 0, 1)
            .unwrap();
        fs.flush().unwrap();
        fs.unlink("/old").unwrap();
        fs.flush().unwrap();
        let base = disk.snapshot();
        let start = recorder.log.lock().unwrap().len();
        fs.write_file("/tmp", &[7u8; 60_000], 0o644, 0, 0, 1)
            .unwrap();
        fs.rename("/tmp", "/final").unwrap();
        fs.flush().unwrap();
        drop(fs);
        let log = recorder.log.lock().unwrap()[start..].to_vec();
        for point in 0..=log.len() {
            let io = MemIo::from_bytes(image_at(&base, &log, point));
            io.set_writable(false);
            let image = Ext2::open(Box::new(io), clock).unwrap();
            workload::walk(&image, &mut |path, data| {
                assert!(
                    data.iter().all(|&byte| byte == 0 || byte == 7),
                    "{block_size}: after write {point}, {path} shows the deleted file's bytes"
                );
            });
        }
    }
}
