//! Every public operation: normal use, edge cases and known-bad input.

use super::*;
use crate::{AttrChange, Ext2Error, FileKind, Owner, S_IFDIR, S_IFREG};

pub(super) const ALICE: Owner = Owner {
    uid: 1000,
    gid: 100,
};

pub(super) fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 + 7) as u8).collect()
}

#[test]
fn create_write_read_roundtrip_at_every_block_size() {
    for block_size in BLOCK_SIZES {
        let (io, fs) = fresh(4 * 1024 * 1024, block_size);
        let meta = fs.create("/hello", 0o640, ALICE).unwrap();
        assert_eq!(
            (meta.mode, meta.uid, meta.gid),
            (S_IFREG | 0o640, 1000, 100)
        );
        assert_eq!(meta.size, 0);
        // Sizes straddling the direct, single- and double-indirect ranges.
        let sizes = [
            1,
            block_size as usize,
            13 * block_size as usize + 5,
            300 * 1024,
        ];
        for (n, len) in sizes.into_iter().enumerate() {
            let path = std::format!("/f{n}");
            let data = pattern(len);
            fs.create(&path, 0o644, Owner::ROOT).unwrap();
            assert_eq!(fs.write(&path, 0, &data).unwrap(), len);
            assert_eq!(fs.lookup(&path).unwrap().size, len as u64);
            assert_eq!(fs.read_file(&path).unwrap(), data, "size {len}");
        }
        assert_clean(&io);
    }
}

#[test]
fn partial_reads_offsets_and_overwrites() {
    let (io, fs) = fresh(2 * 1024 * 1024, 1024);
    fs.create("/f", 0o644, Owner::ROOT).unwrap();
    fs.write("/f", 0, &pattern(5000)).unwrap();
    let mut buf = [0u8; 100];
    assert_eq!(fs.read("/f", 1000, &mut buf).unwrap(), 100);
    assert_eq!(&buf[..], &pattern(5000)[1000..1100]);
    assert_eq!(
        fs.read("/f", 4950, &mut buf).unwrap(),
        50,
        "short at the end"
    );
    assert_eq!(fs.read("/f", 5000, &mut buf).unwrap(), 0);
    assert_eq!(fs.read("/f", 1 << 40, &mut buf).unwrap(), 0);
    assert_eq!(fs.read("/f", 0, &mut []).unwrap(), 0);
    // Overwrite across a block boundary without changing the size.
    fs.write("/f", 1020, b"0123456789").unwrap();
    let mut got = [0u8; 10];
    fs.read("/f", 1020, &mut got).unwrap();
    assert_eq!(&got, b"0123456789");
    assert_eq!(fs.lookup("/f").unwrap().size, 5000);
    assert_clean(&io);
}

#[test]
fn sparse_files_read_zeros_and_allocate_lazily() {
    let (io, fs) = fresh(4 * 1024 * 1024, 1024);
    fs.create("/s", 0o644, Owner::ROOT).unwrap();
    fs.write("/s", 300_000, b"tail").unwrap(); // into the double-indirect range
    assert_eq!(fs.lookup("/s").unwrap().size, 300_004);
    assert_eq!(fs.mapped_block("/s", 0).unwrap(), 0, "a hole has no block");
    assert_ne!(fs.mapped_block("/s", 292).unwrap(), 0);
    let data = fs.read_file("/s").unwrap();
    assert!(data[..300_000].iter().all(|&b| b == 0));
    assert_eq!(&data[300_000..], b"tail");
    assert_clean(&io);
}

#[test]
fn truncate_grows_sparsely_and_shrinks_freeing_blocks() {
    let (io, fs) = fresh(4 * 1024 * 1024, 1024);
    let baseline = fs.free_blocks().unwrap();
    fs.create("/t", 0o644, Owner::ROOT).unwrap();
    fs.write("/t", 0, &pattern(40_000)).unwrap();
    let used = baseline - fs.free_blocks().unwrap();
    assert!(used >= 40, "data plus an indirect table, got {used}");
    fs.truncate("/t", 100).unwrap();
    assert_eq!(fs.lookup("/t").unwrap().size, 100);
    assert_eq!(fs.read_file("/t").unwrap(), pattern(40_000)[..100]);
    // Growing again must read zeros, never the data cut off earlier.
    fs.truncate("/t", 5000).unwrap();
    let data = fs.read_file("/t").unwrap();
    assert_eq!(&data[..100], &pattern(40_000)[..100]);
    assert!(data[100..].iter().all(|&b| b == 0));
    fs.truncate("/t", 0).unwrap();
    assert_eq!(fs.free_blocks().unwrap(), baseline, "everything came back");
    assert_eq!(
        fs.truncate("/t", crate::MAX_FILE_SIZE + 1),
        Err(Ext2Error::NoSpace)
    );
    assert_eq!(fs.truncate("/", 0), Err(Ext2Error::IsDir));
    assert_eq!(fs.truncate("/nope", 0), Err(Ext2Error::NotFound));
    assert_clean(&io);
}

#[test]
fn mkdir_readdir_rmdir_and_link_counts() {
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    assert_eq!(fs.mkdir("/a", 0o755, ALICE).unwrap().mode, S_IFDIR | 0o755);
    fs.mkdir("/a/b", 0o700, ALICE).unwrap();
    fs.create("/a/f", 0o600, ALICE).unwrap();
    assert_eq!(
        fs.link_count("/a").unwrap(),
        3,
        "`.`, the parent's entry and b's `..`"
    );
    assert_eq!(fs.link_count("/").unwrap(), 4);
    let mut listed: Vec<_> = fs
        .readdir("/a")
        .unwrap()
        .into_iter()
        .map(|e| (e.name, e.kind))
        .collect();
    listed.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        listed,
        [("b".into(), FileKind::Dir), ("f".into(), FileKind::File)]
    );
    assert_eq!(fs.rmdir("/a"), Err(Ext2Error::NotEmpty));
    assert_eq!(fs.rmdir("/a/f"), Err(Ext2Error::NotDir));
    assert_eq!(fs.rmdir("/"), Err(Ext2Error::Exists));
    fs.rmdir("/a/b").unwrap();
    assert_eq!(fs.link_count("/a").unwrap(), 2);
    assert_eq!(fs.readdir("/a/f"), Err(Ext2Error::NotDir));
    assert_eq!(fs.readdir("/zzz"), Err(Ext2Error::NotFound));
    assert_clean(&io);
}

#[test]
fn directories_grow_past_one_block_and_reuse_freed_slots() {
    let (io, fs) = fresh(8 * 1024 * 1024, 1024);
    fs.mkdir("/d", 0o755, Owner::ROOT).unwrap();
    for n in 0..200 {
        fs.create(
            &std::format!("/d/file-with-a-long-name-{n:04}"),
            0o644,
            Owner::ROOT,
        )
        .unwrap();
    }
    assert!(
        fs.lookup("/d").unwrap().size > 1024,
        "more than one directory block"
    );
    assert_eq!(fs.readdir("/d").unwrap().len(), 200);
    for n in (0..200).step_by(2) {
        fs.unlink(&std::format!("/d/file-with-a-long-name-{n:04}"))
            .unwrap();
    }
    let size = fs.lookup("/d").unwrap().size;
    for n in 0..100 {
        fs.create(&std::format!("/d/again-{n:04}"), 0o644, Owner::ROOT)
            .unwrap();
    }
    assert_eq!(
        fs.lookup("/d").unwrap().size,
        size,
        "freed records are reused"
    );
    assert_eq!(fs.readdir("/d").unwrap().len(), 200);
    assert_clean(&io);
}

#[test]
fn creation_errors() {
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    fs.create("/f", 0o644, Owner::ROOT).unwrap();
    assert_eq!(fs.create("/f", 0o644, Owner::ROOT), Err(Ext2Error::Exists));
    assert_eq!(fs.mkdir("/f", 0o755, Owner::ROOT), Err(Ext2Error::Exists));
    assert_eq!(
        fs.create("/f/x", 0o644, Owner::ROOT),
        Err(Ext2Error::NotDir)
    );
    assert_eq!(
        fs.create("/missing/x", 0o644, Owner::ROOT),
        Err(Ext2Error::NotFound)
    );
    assert_eq!(fs.create("/", 0o644, Owner::ROOT), Err(Ext2Error::Exists));
    assert_eq!(fs.create("/.", 0o644, Owner::ROOT), Err(Ext2Error::Invalid));
    assert_eq!(
        fs.create("/a/..", 0o644, Owner::ROOT),
        Err(Ext2Error::Invalid)
    );
    let long = "x".repeat(256);
    assert_eq!(
        fs.create(&std::format!("/{long}"), 0o644, Owner::ROOT),
        Err(Ext2Error::NameTooLong)
    );
    fs.create(&std::format!("/{}", "y".repeat(255)), 0o644, Owner::ROOT)
        .unwrap();
    // Owners must fit the 16-bit on-disk fields: truncating 65536 would give the file to root.
    let wide = Owner { uid: 65536, gid: 0 };
    assert_eq!(fs.create("/w", 0o644, wide), Err(Ext2Error::Invalid));
    assert_eq!(fs.mkdir("/w", 0o755, wide), Err(Ext2Error::Invalid));
    assert_eq!(fs.lookup("/w"), Err(Ext2Error::NotFound));
    assert_eq!(fs.read_file("/"), Err(Ext2Error::IsDir));
    assert_eq!(fs.write("/", 0, b"x"), Err(Ext2Error::IsDir));
    assert_clean(&io);
}

#[test]
fn unlink_releases_blocks_and_inode() {
    let (io, fs) = fresh(4 * 1024 * 1024, 1024);
    let (blocks, inodes) = (fs.free_blocks().unwrap(), fs.free_inodes().unwrap());
    fs.create("/u", 0o644, Owner::ROOT).unwrap();
    fs.write("/u", 0, &pattern(30_000)).unwrap();
    assert_eq!(fs.unlink("/"), Err(Ext2Error::Exists));
    fs.mkdir("/dir", 0o755, Owner::ROOT).unwrap();
    assert_eq!(fs.unlink("/dir"), Err(Ext2Error::IsDir));
    fs.unlink("/u").unwrap();
    fs.rmdir("/dir").unwrap();
    assert_eq!(fs.unlink("/u"), Err(Ext2Error::NotFound));
    assert_eq!(
        (fs.free_blocks().unwrap(), fs.free_inodes().unwrap()),
        (blocks, inodes)
    );
    assert_clean(&io);
}

#[test]
fn rename_files_directories_and_replacement() {
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    fs.mkdir("/a", 0o755, Owner::ROOT).unwrap();
    fs.mkdir("/b", 0o755, Owner::ROOT).unwrap();
    fs.write_file("/a/f", b"one", 0o644, 0, 0, 1).unwrap();
    fs.write_file("/a/g", b"two", 0o644, 0, 0, 1).unwrap();
    fs.rename("/a/f", "/b/f").unwrap();
    assert_eq!(fs.read_file("/b/f").unwrap(), b"one");
    assert_eq!(fs.lookup("/a/f"), Err(Ext2Error::NotFound));
    fs.rename("/a/g", "/b/f").unwrap(); // a file replaces a file
    assert_eq!(fs.read_file("/b/f").unwrap(), b"two");
    fs.rename("/b/f", "/b/f").unwrap(); // to itself is a no-op
                                        // A directory moves with its `..`, and the link counts follow.
    fs.mkdir("/a/sub", 0o755, Owner::ROOT).unwrap();
    assert_eq!(
        (fs.link_count("/a").unwrap(), fs.link_count("/b").unwrap()),
        (3, 2)
    );
    fs.rename("/a/sub", "/b/sub").unwrap();
    assert_eq!(
        (fs.link_count("/a").unwrap(), fs.link_count("/b").unwrap()),
        (2, 3)
    );
    // A directory may replace only an empty directory.
    fs.mkdir("/c", 0o755, Owner::ROOT).unwrap();
    fs.write_file("/c/x", b"x", 0o644, 0, 0, 1).unwrap();
    assert_eq!(fs.rename("/b/sub", "/c"), Err(Ext2Error::NotEmpty));
    fs.mkdir("/e", 0o755, Owner::ROOT).unwrap();
    fs.rename("/b/sub", "/e").unwrap();
    assert_eq!(fs.rename("/b/f", "/e"), Err(Ext2Error::IsDir));
    assert_eq!(fs.rename("/e", "/b/f"), Err(Ext2Error::NotDir));
    assert_eq!(
        fs.rename("/c", "/c/inside"),
        Err(Ext2Error::Invalid),
        "into itself"
    );
    assert_eq!(fs.rename("/nope", "/x"), Err(Ext2Error::NotFound));
    assert_clean(&io);
}

#[test]
fn setattr_changes_only_the_selected_fields() {
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    fs.create("/f", 0o644, ALICE).unwrap();
    let meta = fs
        .setattr(
            "/f",
            &AttrChange {
                mode: Some(0o4755),
                ..AttrChange::default()
            },
        )
        .unwrap();
    assert_eq!(
        meta.mode,
        S_IFREG | 0o4755,
        "type bits kept, permission bits replaced"
    );
    assert_eq!((meta.uid, meta.gid), (1000, 100));
    let meta = fs
        .setattr(
            "/f",
            &AttrChange {
                uid: Some(7),
                gid: Some(8),
                atime: Some(100),
                mtime: Some(200),
                ctime: Some(300),
                ..AttrChange::default()
            },
        )
        .unwrap();
    assert_eq!((meta.uid, meta.gid), (7, 8));
    assert_eq!(
        (meta.times.atime, meta.times.mtime, meta.times.ctime),
        (100, 200, 300)
    );
    // Times outside 1970..=2038 are clamped, not wrapped.
    let meta = fs
        .setattr(
            "/f",
            &AttrChange {
                mtime: Some(-5),
                atime: Some(1 << 40),
                ..AttrChange::default()
            },
        )
        .unwrap();
    assert_eq!(
        (meta.times.mtime, meta.times.atime),
        (0, i64::from(i32::MAX))
    );
    // A refused field leaves the inode untouched.
    let before = fs.lookup("/f").unwrap();
    let bad = AttrChange {
        mode: Some(0o600),
        uid: Some(70_000),
        ..AttrChange::default()
    };
    assert_eq!(fs.setattr("/f", &bad), Err(Ext2Error::Invalid));
    assert_eq!(fs.lookup("/f").unwrap(), before);
    assert_eq!(
        fs.setattr("/nope", &AttrChange::default()),
        Err(Ext2Error::NotFound)
    );
    let dir = fs
        .setattr(
            "/",
            &AttrChange {
                mode: Some(0o1777),
                ..AttrChange::default()
            },
        )
        .unwrap();
    assert_eq!(dir.mode, S_IFDIR | 0o1777);
    assert_clean(&io);
}

#[test]
fn timestamps_come_from_the_clock() {
    let (_, fs) = fresh(2 * 1024 * 1024, 4096);
    let meta = fs.create("/f", 0o644, Owner::ROOT).unwrap();
    assert_eq!(
        (meta.times.atime, meta.times.mtime, meta.times.ctime),
        (clock(), clock(), clock())
    );
}
