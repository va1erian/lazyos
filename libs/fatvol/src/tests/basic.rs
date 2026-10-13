//! Names, files, directories, rename, truncate, free space.

use fused::daemon::FuseFs;
use fused::wire::{S_IFDIR, S_IFREG};

use super::*;

#[test]
fn every_fat_type_opens_with_its_label() {
    for fat in [12, 16, 32] {
        let (vol, _) = volume(fat);
        assert_eq!(vol.fat_bits(), fat);
        assert_eq!(vol.label(), "LAZYTEST");
        assert_eq!(vol.volume_id(), 0x1234_5678);
        assert!(!vol.dirty_at_open());
    }
}

#[test]
fn files_round_trip_with_long_names_and_any_case() {
    let (mut vol, _) = volume(16);
    let name = "A long file name with spaces.and.dots.txt";
    put(&mut vol, name, b"hello").unwrap();
    assert_eq!(get(&mut vol, name).unwrap(), b"hello");
    // Case-insensitive lookup, case-preserving names.
    assert_eq!(get(&mut vol, &name.to_uppercase()).unwrap(), b"hello");
    assert_eq!(names(&mut vol, ""), [name]);
    let attr = vol.lookup(path(name)).unwrap();
    assert_eq!(attr.mode, S_IFREG | 0o644);
    assert_eq!((attr.uid, attr.gid, attr.size), (1000, 1000, 5));
    assert_eq!(attr.mtime, NOW);
    // Non-ASCII names survive.
    let accented = "r\u{e9}sum\u{e9} \u{65e5}\u{672c}.doc";
    put(&mut vol, accented, b"x").unwrap();
    assert!(names(&mut vol, "").contains(&String::from(accented)));
}

#[test]
fn directories_nest_and_refuse_what_fat_refuses() {
    let (mut vol, _) = volume(32);
    let dir = vol.mkdir("photos", 0o755, 1000, 1000).unwrap();
    assert_eq!(dir.mode, S_IFDIR | 0o755);
    vol.mkdir("photos/2020", 0o755, 0, 0).unwrap();
    put(&mut vol, "photos/2020/a.jpg", &pattern(10_000, 1)).unwrap();
    assert_eq!(names(&mut vol, "photos"), ["2020"]);
    assert_eq!(names(&mut vol, "photos/2020"), ["a.jpg"]);
    assert_errno(vol.rmdir("photos"), e::ENOTEMPTY);
    assert_errno(vol.unlink("photos"), e::EISDIR);
    assert_errno(vol.rmdir("photos/2020/a.jpg"), e::ENOTDIR);
    assert_errno(vol.mkdir("photos", 0o755, 0, 0), e::EEXIST);
    assert_errno(vol.create("photos/2020/A.JPG", 0o644, 0, 0), e::EEXIST);
    assert_errno(vol.lookup(path("nothing/here")), e::ENOENT);
    // Characters FAT forbids, and a name that is too long.
    assert_errno(vol.create("bad:name", 0o644, 0, 0), e::EINVAL);
    assert_errno(vol.create(&"x".repeat(300), 0o644, 0, 0), e::ENAMETOOLONG);
    vol.unlink("photos/2020/a.jpg").unwrap();
    vol.rmdir("photos/2020").unwrap();
    vol.rmdir("photos").unwrap();
    assert!(names(&mut vol, "").is_empty());
}

#[test]
fn rename_moves_files_and_trees_and_replaces() {
    let (mut vol, _) = volume(32);
    vol.mkdir("a", 0o755, 0, 0).unwrap();
    put(&mut vol, "a/one.txt", b"one").unwrap();
    put(&mut vol, "two.txt", b"two").unwrap();
    let before = vol.lookup(path("a/one.txt")).unwrap().ino;
    vol.rename("a", "b").unwrap();
    // A node handle follows the rename.
    let moved = vol.lookup(path("b/one.txt")).unwrap();
    assert_eq!(moved.ino, before);
    assert_errno(vol.lookup(path("a/one.txt")), e::ENOENT);
    vol.rename("two.txt", "b/one.txt").unwrap();
    assert_eq!(get(&mut vol, "b/one.txt").unwrap(), b"two");
    assert_eq!(names(&mut vol, ""), ["b"]);
    assert_errno(vol.rename("b", "b/inner"), e::EINVAL);
    assert_errno(vol.rename("missing", "x"), e::ENOENT);
    put(&mut vol, "f", b"f").unwrap();
    assert_errno(vol.rename("f", "b"), e::EISDIR);
}

#[test]
fn nodes_name_files_by_inode_and_go_stale() {
    let (mut vol, _) = volume(16);
    put(&mut vol, "n.txt", b"abcdef").unwrap();
    let attr = vol.lookup(path("n.txt")).unwrap();
    let node = Target::Node {
        ino: attr.ino,
        generation: attr.generation,
    };
    let mut out = [0u8; 4];
    assert_eq!(vol.read(node, 2, &mut out).unwrap(), 4);
    assert_eq!(&out, b"cdef");
    vol.unlink("n.txt").unwrap();
    assert_errno(vol.lookup(node), e::ESTALE);
    put(&mut vol, "n.txt", b"new").unwrap();
    assert_errno(vol.lookup(node), e::ESTALE);
}

#[test]
fn truncate_and_sparse_writes_zero_fill() {
    let (mut vol, _) = volume(16);
    put(&mut vol, "t", &pattern(5000, 3)).unwrap();
    vol.truncate(path("t"), 100).unwrap();
    assert_eq!(get(&mut vol, "t").unwrap(), pattern(5000, 3)[..100]);
    vol.truncate(path("t"), 3000).unwrap();
    let grown = get(&mut vol, "t").unwrap();
    assert_eq!(&grown[..100], &pattern(5000, 3)[..100]);
    assert!(grown[100..].iter().all(|&b| b == 0));
    // A write past the end leaves a hole of zeros.
    vol.write(path("t"), 6000, b"tail").unwrap();
    let all = get(&mut vol, "t").unwrap();
    assert_eq!(all.len(), 6004);
    assert!(all[3000..6000].iter().all(|&b| b == 0));
    assert_eq!(&all[6000..], b"tail");
    // Reading at or past the end is empty.
    let mut out = [0u8; 8];
    assert_eq!(vol.read(path("t"), 6004, &mut out).unwrap(), 0);
    assert_eq!(vol.read(path("t"), 9999, &mut out).unwrap(), 0);
    vol.truncate(path("t"), 0).unwrap();
    assert_eq!(vol.lookup(path("t")).unwrap().size, 0);
    assert_errno(vol.read(path(""), 0, &mut out), e::EISDIR);
}

#[test]
fn big_files_cross_the_bypass_path_intact() {
    let (mut vol, _) = volume(32);
    let data = pattern(3 * 1024 * 1024 + 777, 9);
    put(&mut vol, "big.bin", &data).unwrap();
    vol.settle().unwrap();
    assert_eq!(get(&mut vol, "big.bin").unwrap(), data);
    // An unaligned overwrite in the middle.
    vol.write(path("big.bin"), 1_000_001, &[0xAB; 70_000])
        .unwrap();
    let again = get(&mut vol, "big.bin").unwrap();
    assert_eq!(&again[..1_000_001], &data[..1_000_001]);
    assert!(again[1_000_001..1_070_001].iter().all(|&b| b == 0xAB));
    assert_eq!(&again[1_070_001..], &data[1_070_001..]);
}

#[test]
fn free_space_is_reported_and_runs_out_cleanly() {
    let (mut vol, _) = volume(16);
    let before = vol.statfs().unwrap();
    assert_eq!(before.magic, 0x4d44);
    assert_eq!(before.name_max, 255);
    put(&mut vol, "f", &pattern(100_000, 0)).unwrap();
    let after = vol.statfs().unwrap();
    assert!(after.blocks_free < before.blocks_free);
    let used = (before.blocks_free - after.blocks_free) * after.block_size;
    assert!((100_000..100_000 + 2 * after.block_size).contains(&used));
    // Fill the volume: ENOSPC, and everything written before still reads.
    let chunk = pattern(64 * 1024, 5);
    let mut n = 0;
    let err = loop {
        let name = alloc::format!("fill{n}");
        match put(&mut vol, &name, &chunk) {
            Ok(()) => n += 1,
            Err(code) => break code,
        }
        assert!(n < 1000, "never full");
    };
    assert_eq!(err, e::ENOSPC);
    assert_eq!(get(&mut vol, "f").unwrap(), pattern(100_000, 0));
    // Deleting makes room again (the half-written file goes too).
    let _ = vol.unlink(&alloc::format!("fill{n}"));
    for i in 0..n {
        vol.unlink(&alloc::format!("fill{i}")).unwrap();
    }
    put(&mut vol, "again", &chunk).unwrap();
}

#[test]
fn a_read_only_volume_refuses_every_change_and_writes_nothing() {
    let (mut vol, disk) = volume(16);
    put(&mut vol, "keep.txt", b"keep").unwrap();
    vol.settle().unwrap();
    drop(vol);
    disk.reset_counters();
    let mut vol = open(
        &disk,
        Options {
            writable: false,
            ..opts()
        },
    );
    assert_eq!(get(&mut vol, "keep.txt").unwrap(), b"keep");
    assert_eq!(vol.lookup(path("keep.txt")).unwrap().mode, S_IFREG | 0o444);
    assert_errno(vol.create("x", 0o644, 0, 0), e::EROFS);
    assert_errno(vol.mkdir("d", 0o755, 0, 0), e::EROFS);
    assert_errno(vol.write(path("keep.txt"), 0, b"no"), e::EROFS);
    assert_errno(vol.truncate(path("keep.txt"), 0), e::EROFS);
    assert_errno(vol.unlink("keep.txt"), e::EROFS);
    assert_errno(vol.rename("keep.txt", "k"), e::EROFS);
    vol.settle().unwrap();
    assert_eq!(disk.writes(), 0);
}

#[test]
fn utimes_sets_the_modification_time_and_chmod_is_accepted() {
    use fused::payload::{set, SetAttrRecord};
    let (mut vol, _) = volume(16);
    put(&mut vol, "t.txt", b"x").unwrap();
    let change = SetAttrRecord {
        mask: set::MTIME | set::MODE | set::UID,
        mtime: 1_000_000_000,
        mode: 0o600,
        uid: 5,
        ..SetAttrRecord::default()
    };
    let attr = vol.setattr("t.txt", &change).unwrap();
    assert_eq!(attr.mtime, 1_000_000_000);
    assert_eq!((attr.mode & 0o777, attr.uid), (0o644, 1000));
}

#[test]
fn dos_time_round_trips() {
    use crate::clock::{to_datetime, to_unix};
    for unix in [315_532_800, NOW, 951_782_400 /* 2000-02-29 */, 4_354_819_198] {
        let stamp = to_datetime(unix);
        // DOS time has two-second resolution.
        assert!((to_unix(stamp) - unix).abs() <= 1, "{unix} -> {stamp:?}");
    }
    // Before 1980 clamps to 1980-01-01.
    assert_eq!(to_unix(to_datetime(0)), 315_532_800);
}
