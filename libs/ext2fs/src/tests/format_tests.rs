//! The formatter: layout, features, geometry limits and the fresh volume.

use super::*;
use crate::{Ext2Error, FileKind, FsStats, Owner, S_IFDIR, S_IFMT};

#[test]
fn formats_every_block_size_into_a_clean_volume() {
    for block_size in BLOCK_SIZES {
        let (io, fs) = fresh(4 * 1024 * 1024, block_size);
        assert_eq!(fs.block_size(), block_size);
        assert_eq!(fs.uuid(), UUID);
        assert_eq!(&fs.label()[..5], b"test\0");
        assert!(fs.was_clean_at_mount() && !fs.had_errors_at_mount());
        assert_clean(&io);
    }
}

#[test]
fn fresh_volume_has_root_and_lost_found() {
    let (_, fs) = fresh(2 * 1024 * 1024, 4096);
    let root = fs.lookup("/").unwrap();
    assert_eq!(root.kind, FileKind::Dir);
    assert_eq!(root.mode, S_IFDIR | 0o755);
    assert_eq!((root.uid, root.gid), (0, 0));
    assert_eq!(root.times.mtime, clock());
    let names: Vec<_> = fs
        .readdir("/")
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["lost+found"]);
    let lost = fs.lookup("/lost+found").unwrap();
    assert_eq!(lost.mode & 0o7777, 0o700);
    assert_eq!(lost.ino, 11);
    assert_eq!(fs.link_count("/").unwrap(), 3);
    assert_eq!(fs.link_count("/lost+found").unwrap(), 2);
    // 16 KiB of lost+found with 4 KiB blocks is 4 blocks.
    assert_eq!(lost.size, 16 * 1024);
}

#[test]
fn lost_found_is_two_blocks_at_least_and_never_past_the_direct_slots() {
    let (_, small) = fresh(2 * 1024 * 1024, 1024);
    assert_eq!(small.lookup("/lost+found").unwrap().size, 12 * 1024);
    let (_, big) = fresh(2 * 1024 * 1024, 4096);
    assert_eq!(big.lookup("/lost+found").unwrap().size, 16 * 1024);
}

#[test]
fn superblock_matches_what_mke2fs_writes() {
    let io = formatted(8 * 1024 * 1024, 4096);
    let image = io.snapshot();
    let sb = &image[1024..2048];
    let le32 = |at: usize| u32::from_le_bytes(sb[at..at + 4].try_into().unwrap());
    assert_eq!(u16::from_le_bytes([sb[0x38], sb[0x39]]), 0xEF53);
    assert_eq!(le32(0x4C), 1, "revision 1");
    assert_eq!(le32(0x54), 11, "first inode");
    assert_eq!(u16::from_le_bytes([sb[0x58], sb[0x59]]), 128, "inode size");
    assert_eq!(le32(0x5C), 0, "no compat features");
    assert_eq!(le32(0x60), 0x2, "FILETYPE");
    assert_eq!(le32(0x64), 0x3, "SPARSE_SUPER | LARGE_FILE");
    assert_eq!(le32(0x18), 2, "4 KiB blocks");
    assert_eq!(le32(0x14), 0, "no boot block offset");
    assert_eq!(&sb[0x68..0x78], &UUID);
    assert_eq!(u16::from_le_bytes([sb[0x3A], sb[0x3B]]), 1, "clean");
}

#[test]
fn sparse_super_backups_sit_in_groups_one_three_five_seven() {
    // 1 KiB blocks, 8192 per group (8 MiB): ten groups in 80 MiB.
    let io = formatted(80 * 1024 * 1024, 1024);
    let image = io.snapshot();
    for group in 0..10usize {
        let magic_at = (1 + group * 8192) * 1024 + 0x38;
        let has_magic = image[magic_at] == 0x53 && image[magic_at + 1] == 0xEF;
        let backup = [0, 1, 3, 5, 7, 9].contains(&group);
        assert_eq!(has_magic, backup, "group {group}");
    }
    assert_clean(&io);
}

#[test]
fn a_runt_trailing_group_is_dropped_not_formatted() {
    // One full group plus 20 blocks: the tail cannot hold its own metadata.
    let blocks = 32768 + 20;
    let io = MemIo::new(blocks * 4096);
    let geometry = Geometry {
        block_size: 4096,
        blocks_count: blocks as u32,
        bytes_per_inode: 16 * 1024,
    };
    crate::format(&io, geometry, "", UUID, 0).unwrap();
    let fs = open(&io);
    assert_eq!(fs.statfs().unwrap().blocks, 32768);
    assert_clean(&io);
}

#[test]
fn a_multi_group_volume_allocates_across_groups() {
    // 1 KiB blocks, 8 MiB groups: 12 MiB of files cannot fit in group 0.
    let (io, fs) = fresh(24 * 1024 * 1024, 1024);
    let FsStats {
        blocks_free: before,
        ..
    } = fs.statfs().unwrap();
    let data = std::vec![0xAB; 1024 * 1024];
    for n in 0..12 {
        fs.write_file(&std::format!("/f{n}"), &data, 0o644, 0, 0, 1)
            .unwrap();
    }
    assert!(fs.statfs().unwrap().blocks_free <= before - 12 * 1024);
    assert_eq!(fs.read_file("/f11").unwrap(), data);
    let late = fs.mapped_block("/f11", 0).unwrap();
    assert!(
        late > 8192,
        "the last file should live past group 0, got block {late}"
    );
    assert_clean(&io);
}

#[test]
fn format_rejects_bad_arguments() {
    let io = MemIo::new(4 * 1024 * 1024);
    let good = geometry(4 * 1024 * 1024, 4096);
    let long = "seventeen-chars!!";
    assert_eq!(
        crate::format(&io, good, long, UUID, 0),
        Err(Ext2Error::Invalid)
    );
    assert_eq!(
        crate::format(&io, good, "caf\u{e9}", UUID, 0),
        Err(Ext2Error::Invalid)
    );
    for bad in [0, 512, 3000, 8192] {
        let geometry = Geometry {
            block_size: bad,
            ..good
        };
        assert_eq!(
            crate::format(&io, geometry, "", UUID, 0),
            Err(Ext2Error::Invalid),
            "{bad}"
        );
    }
    let tiny = Geometry {
        blocks_count: 100,
        ..good
    };
    assert_eq!(
        crate::format(&io, tiny, "", UUID, 0),
        Err(Ext2Error::Invalid)
    );
    let no_inodes = Geometry {
        bytes_per_inode: 0,
        ..good
    };
    assert_eq!(
        crate::format(&io, no_inodes, "", UUID, 0),
        Err(Ext2Error::Invalid)
    );
    // Larger than the device.
    let large = Geometry {
        blocks_count: good.blocks_count + 1,
        ..good
    };
    assert_eq!(
        crate::format(&io, large, "", UUID, 0),
        Err(Ext2Error::Invalid)
    );
    // More groups than the driver bounds.
    let huge = MemIo::new(1024 * 1024);
    let groups = Geometry {
        blocks_count: u32::MAX,
        ..good
    };
    assert_eq!(
        crate::format(&huge, groups, "", UUID, 0),
        Err(Ext2Error::Invalid)
    );
    // A read-only device.
    io.set_writable(false);
    assert_eq!(
        crate::format(&io, good, "", UUID, 0),
        Err(Ext2Error::ReadOnly)
    );
}

#[test]
fn format_reports_a_device_that_dies() {
    let io = MemIo::new(2 * 1024 * 1024);
    io.fail_writes_after(10);
    let result = crate::format(&io, geometry(2 * 1024 * 1024, 4096), "", UUID, 0);
    assert_eq!(result, Err(Ext2Error::Io));
}

#[test]
fn for_size_uses_four_kib_blocks() {
    let geometry = Geometry::for_size(512 * 1024 * 1024);
    assert_eq!(geometry.block_size, 4096);
    assert_eq!(geometry.blocks_count, 131072);
    let io = MemIo::new(512 * 1024 * 1024);
    crate::format(&io, geometry, "os", UUID, 0).unwrap();
    let fs = open(&io);
    fs.mkdir("/system", 0o755, Owner { uid: 0, gid: 0 })
        .unwrap();
    assert_eq!(fs.lookup("/system").unwrap().mode & S_IFMT, S_IFDIR);
}
