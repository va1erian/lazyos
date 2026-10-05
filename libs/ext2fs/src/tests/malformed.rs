//! Hostile and damaged images: every one must give an error (at mount or on
//! first use) and none may panic or loop.

use super::*;
use crate::memio::MemIo;
use crate::{Ext2Error, Owner};

const SB: usize = 1024;

/// A formatted 4 KiB-block image with `edit` applied to its superblock.
fn with_superblock(edit: impl FnOnce(&mut [u8])) -> MemIo {
    let io = formatted(4 * 1024 * 1024, 4096);
    io.with_bytes(|image| edit(&mut image[SB..SB + 1024]));
    io
}

fn set32(sb: &mut [u8], at: usize, value: u32) {
    sb[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn open_err(io: &MemIo) -> Ext2Error {
    Ext2::open(Box::new(io.clone()), clock)
        .err()
        .expect("the image must be refused")
}

/// Like [`open_err`], but names the damage when the image is wrongly accepted.
fn refused(io: &MemIo, what: &str) -> Ext2Error {
    match Ext2::open(Box::new(io.clone()), clock) {
        Err(error) => error,
        Ok(_) => panic!("{what} was accepted"),
    }
}

#[test]
fn truncated_devices_are_refused() {
    assert_eq!(open_err(&MemIo::new(0)), Ext2Error::Invalid);
    assert_eq!(open_err(&MemIo::new(1024)), Ext2Error::Invalid);
    assert_eq!(open_err(&MemIo::new(2047)), Ext2Error::Invalid);
    // A valid superblock on a device that is shorter than the volume it claims.
    let full = formatted(4 * 1024 * 1024, 4096).snapshot();
    let half = MemIo::from_bytes(full[..full.len() / 2].to_vec());
    assert_eq!(open_err(&half), Ext2Error::Invalid);
    assert_eq!(
        open_err(&MemIo::new(1 << 20)),
        Ext2Error::Invalid,
        "no magic"
    );
}

#[test]
fn out_of_range_geometry_is_refused() {
    let cases: [(&str, usize, u32); 14] = [
        ("log block size 3", 0x18, 3),
        ("log block size huge", 0x18, u32::MAX),
        ("first data block 2", 0x14, 2),
        ("zero blocks per group", 0x20, 0),
        ("zero inodes per group", 0x28, 0),
        ("zero blocks", 0x04, 0),
        ("blocks past the device", 0x04, 1 << 30),
        ("no inodes", 0x00, 1),
        ("free blocks past total", 0x0C, u32::MAX),
        ("free inodes past total", 0x10, u32::MAX),
        ("first inode 0", 0x54, 0),
        ("first inode past the table", 0x54, u32::MAX),
        ("more inode groups than block groups", 0x00, 1 << 20),
        ("a ragged inode table", 0x28, 33),
    ];
    for (what, at, value) in cases {
        let io = with_superblock(|sb| set32(sb, at, value));
        let error = refused(&io, what);
        assert!(
            matches!(error, Ext2Error::Invalid | Ext2Error::NotSupported),
            "{what}: {error:?}"
        );
    }
    // More block groups than the driver bounds every per-group loop by.
    let io = formatted(8 * 1024 * 1024, 1024);
    io.with_bytes(|image| set32(&mut image[SB..SB + 1024], 0x20, 1));
    assert_eq!(refused(&io, "8191 groups"), Ext2Error::Invalid);
}

#[test]
fn bad_inode_sizes_are_refused() {
    for size in [0u16, 1, 64, 100, 127, 129, 4097, 8192] {
        let io = with_superblock(|sb| sb[0x58..0x5A].copy_from_slice(&size.to_le_bytes()));
        assert_eq!(open_err(&io), Ext2Error::Invalid, "inode size {size}");
    }
}

#[test]
fn unknown_feature_bits_are_not_supported() {
    // Every incompat bit but FILETYPE, and every ro_compat bit but the two we know.
    for bit in [0x1, 0x4, 0x8, 0x10, 0x40, 0x80, 0x100, 0x200, 0x400, 0x1000] {
        let io = with_superblock(|sb| set32(sb, 0x60, 0x2 | bit));
        assert_eq!(open_err(&io), Ext2Error::NotSupported, "incompat {bit:#x}");
    }
    for bit in [0x4, 0x8, 0x10, 0x20, 0x40, 0x80] {
        let io = with_superblock(|sb| set32(sb, 0x64, 0x3 | bit));
        assert_eq!(open_err(&io), Ext2Error::NotSupported, "ro_compat {bit:#x}");
    }
    // Compat bits (dir_index, ...) do not change how blocks are read.
    let ok = with_superblock(|sb| set32(sb, 0x5C, 0x38));
    assert!(Ext2::open(Box::new(ok), clock).is_ok());
    // The journal bit does: a volume that claims a journal it does not have
    // (no journal inode) is refused rather than mounted without one.
    let claimed = with_superblock(|sb| set32(sb, 0x5C, 0x4));
    assert_eq!(open_err(&claimed), Ext2Error::NotSupported);
}

#[test]
fn a_revision_zero_volume_uses_the_old_defaults() {
    let io = with_superblock(|sb| {
        set32(sb, 0x4C, 0); // revision 0: first inode 11, inode size 128, no features
        set32(sb, 0x60, 0);
        set32(sb, 0x64, 0);
    });
    let fs = Ext2::open(Box::new(io), clock).unwrap();
    assert_eq!(fs.lookup("/").unwrap().ino, 2);
    assert!(fs.readdir("/").is_ok());
}

/// Byte offset of inode `ino` in a 4 KiB-block image (group 0's table).
fn inode_offset(io: &MemIo, ino: usize) -> usize {
    io.with_bytes(|image| {
        let table = u32::from_le_bytes(image[4096 + 8..4096 + 12].try_into().unwrap()) as usize;
        table * 4096 + (ino - 1) * 128
    })
}

fn poke(io: &MemIo, at: usize, bytes: &[u8]) {
    io.with_bytes(|image| image[at..at + bytes.len()].copy_from_slice(bytes));
}

#[test]
fn damaged_group_descriptors_fail_on_use_not_at_mount() {
    // Descriptor 0 sits at block 1: bitmap, bitmap, table, then the counters.
    for (field, name) in [
        (0usize, "block bitmap"),
        (4, "inode bitmap"),
        (8, "inode table"),
    ] {
        let io = formatted(4 * 1024 * 1024, 4096);
        poke(&io, 4096 + field, &0xFFFF_FFF0u32.to_le_bytes());
        let fs = Ext2::open(Box::new(io.clone()), clock).expect("descriptors are read lazily");
        let all = [
            fs.lookup("/").map(drop),
            fs.create("/f", 0o644, Owner::ROOT).map(drop),
            fs.mkdir("/d", 0o755, Owner::ROOT).map(drop),
            fs.write("/f", 0, b"x").map(drop),
        ];
        assert!(all.iter().all(Result::is_err), "{name}: {all:?}");
    }
}

#[test]
fn lying_free_counters_do_not_corrupt_the_bitmaps() {
    let io = formatted(4 * 1024 * 1024, 4096);
    // Claim group 0 has no free blocks and then 65535 free inodes.
    poke(&io, 4096 + 0x0C, &0u16.to_le_bytes());
    let fs = open(&io);
    assert_eq!(
        fs.write_file("/f", b"x", 0o644, 0, 0, 0),
        Err(Ext2Error::NoSpace)
    );
    poke(&io, 4096 + 0x0C, &0xFFFFu16.to_le_bytes());
    poke(&io, 4096 + 0x0E, &0xFFFFu16.to_le_bytes());
    let _ = fs.write_file("/g", &std::vec![1u8; 20_000], 0o644, 0, 0, 0);
    let _ = fs.mkdir("/h", 0o755, Owner::ROOT);
}

#[test]
fn a_descriptor_table_that_runs_off_the_volume_is_refused() {
    // 32 bytes per group: claim so many groups that the table leaves the volume.
    let io = with_superblock(|sb| {
        set32(sb, 0x04, 4096 * 8 * 3000); // blocks
        set32(sb, 0x28, 128);
    });
    assert!(matches!(open_err(&io), Ext2Error::Invalid));
}

#[test]
fn broken_directory_records_are_errors() {
    let corruptions: [(&str, usize, &[u8]); 4] = [
        ("zero record length", 4, &[0, 0]),
        ("record length not a multiple of four", 4, &[13, 0]),
        ("record past the block end", 4, &[0xFF, 0xFF]),
        ("name longer than its record", 6, &[200]),
    ];
    for (what, at, bytes) in corruptions {
        let (io, fs) = fresh(2 * 1024 * 1024, 4096);
        fs.mkdir("/d", 0o755, Owner::ROOT).unwrap();
        let block = fs.first_dir_block("/d");
        poke(&io, block * 4096 + at, bytes);
        assert_eq!(fs.readdir("/d"), Err(Ext2Error::Invalid), "{what}");
        assert_eq!(fs.lookup("/d/x"), Err(Ext2Error::Invalid), "{what}");
        assert_eq!(
            fs.create("/d/x", 0o644, Owner::ROOT),
            Err(Ext2Error::Invalid),
            "{what}"
        );
        assert_eq!(fs.rmdir("/d"), Err(Ext2Error::Invalid), "{what}");
    }
}

impl Ext2 {
    /// The first block of directory `path`, read from its inode.
    fn first_dir_block(&self, path: &str) -> usize {
        let meta = self.lookup(path).unwrap();
        assert_eq!(meta.kind, crate::FileKind::Dir);
        let _guard = self.lock.lock();
        let inode = self.read_inode(meta.ino as u32).unwrap();
        crate::layout::le32(&inode, crate::layout::INO_BLOCK) as usize
    }
}

#[test]
fn a_directory_that_is_its_own_ancestor_is_an_error_not_a_loop() {
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    fs.mkdir("/a", 0o755, Owner::ROOT).unwrap();
    fs.mkdir("/a/b", 0o755, Owner::ROOT).unwrap();
    // Point /a's `..` at /a/b: a cycle in the parent chain.
    let a = fs.first_dir_block("/a");
    let b = fs.lookup("/a/b").unwrap().ino as u32;
    poke(&io, a * 4096 + 12, &b.to_le_bytes());
    assert_eq!(fs.rename("/a", "/a/b/a"), Err(Ext2Error::Invalid));
    let looped = fs.rename("/lost+found", "/a/b/lf");
    assert!(looped.is_err(), "{looped:?}");
}

#[test]
fn remove_tree_stops_at_a_directory_entry_that_loops_back() {
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    fs.mkdir_p("/a/b", 0o755, 0, 0).unwrap();
    fs.write_file("/a/b/x", b"x", 0o644, 0, 0, 0).unwrap();
    // Retarget b's entry `x` (after `.` and `..`) at /a, typed as a directory.
    let b = fs.first_dir_block("/a/b");
    let a = fs.lookup("/a").unwrap().ino as u32;
    poke(&io, b * 4096 + 24, &a.to_le_bytes());
    poke(&io, b * 4096 + 24 + 7, &[2]);
    assert_eq!(fs.remove_tree("/a"), Err(Ext2Error::Invalid));
}

#[test]
fn entries_naming_missing_inodes_are_errors() {
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    fs.write_file("/f", b"x", 0o644, 0, 0, 0).unwrap();
    let root = fs.first_dir_block("/");
    // Rewrite the `lost+found` entry (the one after `.` and `..`) to inode 0x7FFF_FFFF.
    poke(&io, root * 4096 + 24, &0x7FFF_FFFFu32.to_le_bytes());
    assert_eq!(fs.lookup("/lost+found"), Err(Ext2Error::Invalid));
    assert!(fs.readdir("/").is_err() || fs.readdir("/").unwrap().iter().all(|e| e.ino != 0));
}

#[test]
fn self_referencing_block_maps_do_not_loop() {
    let (io, fs) = fresh(2 * 1024 * 1024, 1024);
    fs.write_file("/f", &std::vec![9u8; 40_000], 0o644, 0, 0, 0)
        .unwrap();
    let ino = fs.lookup("/f").unwrap().ino as usize;
    let at = inode_offset_1k(&io, ino);
    // Point the single-indirect slot, then the double and triple, at block 2 (the descriptors).
    for slot in [12usize, 13, 14] {
        poke(&io, at + 0x28 + slot * 4, &2u32.to_le_bytes());
    }
    let _ = fs.read_file("/f");
    let mut buf = std::vec![0u8; 1 << 20];
    let _ = fs.read("/f", 300_000, &mut buf);
    let _ = fs.truncate("/f", 1000);
    let _ = fs.unlink("/f");
    let _ = fs.remove_tree("/f");
}

/// Byte offset of inode `ino` in a 1 KiB-block image (descriptors at block 2).
fn inode_offset_1k(io: &MemIo, ino: usize) -> usize {
    io.with_bytes(|image| {
        let table = u32::from_le_bytes(image[2048 + 8..2048 + 12].try_into().unwrap()) as usize;
        table * 1024 + (ino - 1) * 128
    })
}

#[test]
fn symlinks_and_devices_are_not_supported() {
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    fs.write_file("/f", b"x", 0o644, 0, 0, 0).unwrap();
    let ino = fs.lookup("/f").unwrap().ino as usize;
    let at = inode_offset(&io, ino);
    poke(&io, at, &(0o120777u16).to_le_bytes()); // a symlink
    assert_eq!(fs.lookup("/f"), Err(Ext2Error::NotSupported));
    assert_eq!(fs.read("/f", 0, &mut [0u8; 4]), Err(Ext2Error::IsDir));
    poke(&io, at, &(0o060644u16).to_le_bytes()); // a block device
    assert_eq!(fs.lookup("/f"), Err(Ext2Error::NotSupported));
}
