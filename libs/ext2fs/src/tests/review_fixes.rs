//! Regression tests for the F2 review findings: hostile geometry, runaway
//! orphan release, `..` cycles, rename rollback, bounded scans and the small
//! leak and header fixes.

use super::*;
use crate::layout::*;
use crate::memio::MemIo;
use crate::{Ext2Error, FileKind, OrphanReport, Owner};

const SB: usize = 1024;

fn set32(image: &mut [u8], at: usize, value: u32) {
    image[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn formatted_dense(bytes: u64, block_size: u32, bytes_per_inode: u32) -> MemIo {
    let io = MemIo::new(bytes as usize);
    let geometry = crate::Geometry {
        bytes_per_inode,
        ..geometry(bytes, block_size)
    };
    crate::format(&io, geometry, "test", UUID, clock()).expect("format");
    io
}

fn open_result(io: &MemIo) -> Result<Ext2, Ext2Error> {
    Ext2::open(Box::new(io.clone()), clock)
}

/// Byte offset of inode `ino` in a 1 KiB-block image (group 0's table).
fn inode_at_1k(io: &MemIo, ino: usize) -> usize {
    io.with_bytes(|image| {
        let table = u32::from_le_bytes(image[2048 + 8..2048 + 12].try_into().unwrap()) as usize;
        table * 1024 + (ino - 1) * 128
    })
}

#[test]
fn a_group_larger_than_its_bitmap_block_is_refused_at_mount() {
    // (field offset, value): more bits per group than one bitmap block holds.
    for (what, at, value) in [
        ("blocks per group 8193", SB + 0x20, 8193),
        ("blocks per group huge", SB + 0x20, 1 << 20),
        ("inodes per group 16384", SB + 0x28, 16384),
    ] {
        let io = formatted(4 * 1024 * 1024, 1024);
        io.with_bytes(|image| set32(image, at, value));
        assert_eq!(open_result(&io).err(), Some(Ext2Error::Invalid), "{what}");
    }
}

#[test]
fn the_first_data_block_must_match_the_block_size() {
    let io = formatted(4 * 1024 * 1024, 1024);
    io.with_bytes(|image| set32(image, SB + 0x14, 0));
    assert_eq!(open_result(&io).err(), Some(Ext2Error::Invalid));
    let io = formatted(4 * 1024 * 1024, 4096);
    io.with_bytes(|image| set32(image, SB + 0x14, 1));
    assert_eq!(open_result(&io).err(), Some(Ext2Error::Invalid));
}

#[test]
fn bitmap_helpers_refuse_a_bit_past_the_buffer() {
    let mut buf = [0u8; 4];
    assert_eq!(Ext2::bitmap_test(&buf, 31), Ok(false));
    assert_eq!(Ext2::bitmap_test(&buf, 32), Err(Ext2Error::Invalid));
    assert_eq!(Ext2::bitmap_set(&mut buf, 32), Err(Ext2Error::Invalid));
    assert_eq!(Ext2::bitmap_clear(&mut buf, 99), Err(Ext2Error::Invalid));
    buf.fill(0xFF);
    assert_eq!(Ext2::bitmap_find_zero(&buf, 0, 33), Err(Ext2Error::Invalid));
}

#[test]
fn a_table_whose_entries_all_name_itself_is_released_quickly() {
    let io = formatted(2 * 1024 * 1024, 1024);
    let fs = open(&io);
    let data: Vec<u8> = (0..20_000u32).map(|n| n as u8).collect();
    fs.write_file("/.unlinked-1", &data, 0o644, 0, 0, 1)
        .unwrap();
    let table = fs.mapped_block("/.unlinked-1", 0).unwrap();
    let at = inode_at_1k(&io, fs.lookup("/.unlinked-1").unwrap().ino as usize);
    drop(fs); // unclean stop
    io.with_bytes(|image| {
        // The triple-indirect slot (and only it) names `table`, whose 256
        // entries all name `table` again: 256^3 visits for a naive walk.
        for slot in 0..14 {
            set32(image, at + INO_BLOCK + slot * 4, 0);
        }
        set32(image, at + INO_BLOCK + 14 * 4, table);
        for entry in 0..256 {
            set32(image, table as usize * 1024 + entry * 4, table);
        }
    });
    let fs = open(&io);
    let report = fs.reclaim_orphans(".unlinked-");
    assert_eq!((report.reclaimed, report.failed.len()), (1, 0));
    assert_eq!(fs.lookup("/.unlinked-1"), Err(Ext2Error::NotFound));
}

/// Point directory `path`'s `..` at inode `parent`, whatever it was.
fn force_dotdot(fs: &Ext2, path: &str, parent: u32) {
    let ino = fs.lookup(path).unwrap().ino as u32;
    let _guard = fs.lock.lock();
    let mut inode = fs.read_inode(ino).unwrap();
    fs.set_dotdot(ino, &mut inode, parent).unwrap();
}

#[test]
fn directories_whose_dotdot_entries_chase_each_other_end_the_walk() {
    let (io, fs) = fresh(2 * 1024 * 1024, 1024);
    for dir in ["/a", "/b", "/d"] {
        fs.mkdir(dir, 0o755, Owner::ROOT).unwrap();
    }
    let a = fs.lookup("/a").unwrap().ino as u32;
    let b = fs.lookup("/b").unwrap().ino as u32;
    force_dotdot(&fs, "/a", b);
    force_dotdot(&fs, "/b", a);
    assert_eq!(fs.rename("/d", "/a/d"), Err(Ext2Error::Invalid));
    // The same walk, the other direction.
    assert_eq!(fs.rename("/d", "/b/d"), Err(Ext2Error::Invalid));
    drop(io);
}

/// Fill the volume to its last free block with one-byte files.
fn exhaust_blocks(fs: &Ext2) {
    let _ = fs.create("/pad", 0o644, Owner::ROOT);
    let chunk = [7u8; 1024];
    let mut offset = 0u64;
    while fs.write("/pad", offset, &chunk).is_ok() {
        offset += 1024;
    }
    for n in 0..64 {
        if fs.free_blocks().unwrap() == 0 {
            return;
        }
        let name = std::format!("/q{n}");
        let _ = fs.create(&name, 0o644, Owner::ROOT);
        let _ = fs.write(&name, 0, &[1]);
    }
    assert_eq!(fs.free_blocks().unwrap(), 0, "the volume must be full");
}

#[test]
fn a_cross_directory_rename_that_runs_out_of_space_changes_nothing() {
    let io = formatted_dense(4 * 1024 * 1024, 1024, 4096);
    let fs = open(&io);
    fs.mkdir_p("/a/sub", 0o755, 0, 0).unwrap();
    fs.mkdir("/b", 0o755, Owner::ROOT).unwrap();
    exhaust_blocks(&fs);
    // Fill /b's only block until the next short name no longer fits.
    for n in 0.. {
        if fs
            .create(&std::format!("/b/f{n:04}"), 0o644, Owner::ROOT)
            .is_err()
        {
            break;
        }
    }
    let (a_links, b_links) = (fs.link_count("/a").unwrap(), fs.link_count("/b").unwrap());
    let long = "/b/a_destination_name_far_too_long_for_the_slack_left";
    assert_eq!(fs.rename("/a/sub", long), Err(Ext2Error::NoSpace));
    assert_eq!(fs.link_count("/a").unwrap(), a_links);
    assert_eq!(fs.link_count("/b").unwrap(), b_links);
    assert_eq!(fs.lookup("/a/sub").unwrap().kind, FileKind::Dir);
    assert_eq!(fs.lookup(long), Err(Ext2Error::NotFound));
    fs.flush().unwrap();
    assert_clean(&io); // `..` and every link count still agree
}

#[test]
fn a_same_directory_rename_that_grows_the_directory_keeps_the_new_block() {
    let io = formatted_dense(4 * 1024 * 1024, 1024, 4096);
    let fs = open(&io);
    fs.mkdir("/d", 0o755, Owner::ROOT).unwrap();
    fs.create("/d/x", 0o644, Owner::ROOT).unwrap();
    // Fill the directory block, then rename to a name that needs a new block.
    let mut n = 0;
    while fs.lookup("/d").unwrap().size == 1024 {
        fs.create(&std::format!("/d/f{n:04}"), 0o644, Owner::ROOT)
            .unwrap();
        n += 1;
    }
    let long = "y".repeat(200);
    fs.rename("/d/x", &std::format!("/d/{long}")).unwrap();
    fs.flush().unwrap();
    assert_clean(&io);
    assert!(fs.lookup(&std::format!("/d/{long}")).is_ok());
}

#[test]
fn a_crowd_of_directories_stops_the_orphan_scan_at_its_budget() {
    let io = formatted_dense(16 * 1024 * 1024, 1024, 2048);
    let fs = open(&io);
    for n in 0..crate::MAX_SCAN_DIRS + 200 {
        fs.mkdir(&std::format!("/d{n}"), 0o755, Owner::ROOT)
            .unwrap();
    }
    drop(fs);
    let report = open(&io).reclaim_orphans(".unlinked-");
    assert!(report.scan_truncated);
    assert_eq!(report.reclaimed, 0);
}

#[test]
fn the_orphan_failure_list_is_capped() {
    let io = formatted_dense(2 * 1024 * 1024, 1024, 4096);
    let fs = open(&io);
    let total = crate::orphans::MAX_FAILED + 40;
    for n in 0..total {
        fs.create(&std::format!("/.unlinked-{n}"), 0o644, Owner::ROOT)
            .unwrap();
    }
    let inodes: Vec<u64> = fs.readdir("/").unwrap().iter().map(|e| e.ino).collect();
    drop(fs);
    // An inode with no valid file mode cannot be deleted: every entry fails.
    io.with_bytes(|image| {
        for ino in inodes {
            let at = {
                let table =
                    u32::from_le_bytes(image[2048 + 8..2048 + 12].try_into().unwrap()) as usize;
                table * 1024 + (ino as usize - 1) * 128
            };
            image[at..at + 2].fill(0);
        }
    });
    let report: OrphanReport = open(&io).reclaim_orphans(".unlinked-");
    assert_eq!(report.reclaimed, 0);
    assert_eq!(report.failed.len(), crate::orphans::MAX_FAILED);
}

#[test]
fn mkdir_under_a_full_link_count_leaks_nothing() {
    let io = formatted(2 * 1024 * 1024, 1024);
    let fs = open(&io);
    fs.mkdir("/p", 0o755, Owner::ROOT).unwrap();
    let at = inode_at_1k(&io, fs.lookup("/p").unwrap().ino as usize);
    io.with_bytes(|image| image[at + INO_LINKS..at + INO_LINKS + 2].fill(0xFF));
    let (blocks, inodes) = (fs.free_blocks().unwrap(), fs.free_inodes().unwrap());
    assert_eq!(
        fs.mkdir("/p/x", 0o755, Owner::ROOT),
        Err(Ext2Error::Invalid)
    );
    assert_eq!(
        (fs.free_blocks().unwrap(), fs.free_inodes().unwrap()),
        (blocks, inodes)
    );
}

/// The type byte of the entry called `name` in the first block of `dir`.
fn type_byte(io: &MemIo, fs: &Ext2, dir: &str, name: &[u8]) -> u8 {
    let ino = fs.lookup(dir).unwrap().ino as u32;
    let block = {
        let _guard = fs.lock.lock();
        le32(&fs.read_inode(ino).unwrap(), INO_BLOCK) as usize
    };
    io.with_bytes(|image| {
        let mut offset = block * 1024;
        let end = offset + 1024;
        while offset < end {
            let rec_len = le16(image, offset + DE_REC_LEN) as usize;
            let len = image[offset + DE_NAME_LEN] as usize;
            if le32(image, offset + DE_INO) != 0
                && &image[offset + DE_HEADER..offset + DE_HEADER + len] == name
            {
                return image[offset + DE_FILE_TYPE];
            }
            offset += rec_len;
        }
        panic!("no entry {name:?}");
    })
}

#[test]
fn without_the_filetype_feature_entries_carry_no_type_byte() {
    let io = formatted(2 * 1024 * 1024, 1024);
    io.with_bytes(|image| set32(image, SB + SB_FEATURE_INCOMPAT, 0));
    let fs = open(&io);
    fs.mkdir("/d", 0o755, Owner::ROOT).unwrap();
    fs.create("/f", 0o644, Owner::ROOT).unwrap();
    fs.rename("/f", "/d/g").unwrap();
    assert_eq!(type_byte(&io, &fs, "/", b"d"), 0);
    assert_eq!(type_byte(&io, &fs, "/d", b"g"), 0);
    assert_eq!(type_byte(&io, &fs, "/d", b"."), 0);
    assert_eq!(type_byte(&io, &fs, "/d", b".."), 0);
    // And with the feature on, the types are still written.
    let (io, fs) = fresh(2 * 1024 * 1024, 1024);
    fs.mkdir("/d", 0o755, Owner::ROOT).unwrap();
    assert_eq!(type_byte(&io, &fs, "/", b"d"), FT_DIRECTORY);
    assert_eq!(type_byte(&io, &fs, "/d", b".."), FT_DIRECTORY);
}

#[test]
fn a_tiny_bytes_per_inode_clamps_instead_of_wrapping() {
    let plan = crate::geometry::plan(&crate::Geometry {
        block_size: 4096,
        blocks_count: 1 << 20, // 4 GiB: 2^32 wanted inodes at one byte each
        bytes_per_inode: 1,
    })
    .expect("a plan");
    assert_eq!(plan.inodes_per_group, 4096 * 8);
}
