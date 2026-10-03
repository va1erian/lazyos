//! `Ext2::repair`: each inconsistency a crash can leave, built by hand, is
//! repaired without losing or changing anything reachable; every other kind
//! of damage is refused with nothing written.

use std::collections::BTreeMap;

use super::ops::pattern;
use super::workload;
use super::*;
use crate::layout::*;
use crate::{Owner, Recovery, RepairError, RepairReport, ORPHAN_PREFIX};

/// A flushed volume holding a small tree (one file reaching the indirect
/// blocks), still mounted for the test to damage through the internals.
fn tree(block_size: u32) -> (MemIo, Ext2) {
    let (io, fs) = fresh(4 << 20, block_size);
    for dir in ["/a", "/a/sub", "/b"] {
        fs.mkdir(dir, 0o755, Owner::ROOT).unwrap();
    }
    fs.write_file("/a/one", &pattern(5000), 0o644, 0, 0, 1)
        .unwrap();
    fs.write_file("/a/sub/two", &pattern(70_000), 0o644, 0, 0, 1)
        .unwrap();
    fs.write_file("/b/three", &pattern(100), 0o644, 0, 0, 1)
        .unwrap();
    fs.write_file(
        "/top",
        &pattern(2 * block_size as usize + 1),
        0o644,
        0,
        0,
        1,
    )
    .unwrap();
    fs.flush().unwrap();
    (io, fs)
}

/// Every reachable live file's bytes, by inode number (a path may change: a
/// file can move to lost+found, a directory lose its extra name).
pub(super) fn contents(fs: &Ext2) -> BTreeMap<u64, Vec<u8>> {
    let mut found = BTreeMap::new();
    workload::walk(fs, &mut |path, data| {
        if fs.link_count(path).is_ok_and(|links| links > 0) {
            let ino = fs.lookup(path).unwrap().ino;
            found.insert(ino, data.to_vec());
        }
    });
    found
}

fn ino(fs: &Ext2, path: &str) -> u32 {
    fs.resolve(path).unwrap()
}

/// Recover the damaged volume (after the damage, `fs` is dropped unflushed)
/// and check the outcome: clean by the checker, flagged clean after a flush,
/// and every file reachable before holds the same bytes.
fn recovered(io: &MemIo, fs: Ext2) -> RepairReport {
    let before = contents(&fs);
    drop(fs);
    let mut fs = open(io);
    let repairs = match fs.recover(ORPHAN_PREFIX).unwrap() {
        Recovery::Recovered { repairs, .. } => repairs,
        other => panic!("not recovered: {other:?}"),
    };
    fs.flush().unwrap();
    assert_eq!(io.snapshot()[1024 + 0x3A], 1, "not marked clean");
    assert_clean(io);
    let after = contents(&open(io));
    for (ino, data) in &before {
        assert!(after.get(ino) == Some(data), "inode {ino} lost or changed");
    }
    assert!(!repairs.is_empty());
    repairs
}

/// The damage is refused: `repair` writes nothing and `recover` leaves the
/// volume flagged, quoting the checker and the refusal.
fn refused(io: &MemIo, fs: Ext2, why: &str) {
    drop(fs);
    let before = io.snapshot();
    match open(io).repair() {
        Err(RepairError::Refused(reason)) => assert!(reason.contains(why), "{reason}"),
        other => panic!("damage repaired: {other:?}"),
    }
    assert!(io.snapshot() == before, "a refused repair wrote");
    let mut fs = open(io);
    match fs.recover(ORPHAN_PREFIX).unwrap() {
        Recovery::StillUnclean(reason) => {
            assert!(
                reason.contains("fsck found") && reason.contains("not repaired"),
                "{reason}"
            )
        }
        other => panic!("damage recovered: {other:?}"),
    }
    fs.flush().unwrap();
    assert_eq!(io.snapshot()[1024 + 0x3A] & 1, 0);
}

/// Remove `name` from `dir` and nothing else: the inode it named stays.
fn drop_name(fs: &Ext2, dir: &str, name: &str) -> u32 {
    let parent = ino(fs, dir);
    let mut inode = fs.read_inode(parent).unwrap();
    fs.remove_entry(parent, &mut inode, name).unwrap()
}

fn edit_inode(fs: &Ext2, ino: u32, edit: impl FnOnce(&mut [u8; INODE_CORE_SIZE])) {
    let mut inode = fs.read_inode(ino).unwrap();
    edit(&mut inode);
    fs.write_inode(ino, &inode).unwrap();
}

/// Set or clear `block`'s bit in its group's bitmap.
pub(super) fn set_block_bit(fs: &Ext2, block: u32, used: bool) {
    let index = block - fs.first_data_block;
    let desc = fs.read_group(index / fs.blocks_per_group).unwrap();
    let mut bitmap = std::vec![0u8; fs.block_size as usize];
    fs.read_block(u64::from(desc.block_bitmap), &mut bitmap)
        .unwrap();
    let bit = index % fs.blocks_per_group;
    if used {
        Ext2::bitmap_set(&mut bitmap, bit).unwrap();
    } else {
        Ext2::bitmap_clear(&mut bitmap, bit).unwrap();
    }
    fs.write_block(u64::from(desc.block_bitmap), &bitmap)
        .unwrap();
}

#[test]
fn leaked_blocks_are_freed() {
    for block_size in BLOCK_SIZES {
        let (io, fs) = tree(block_size);
        let free = fs.blocks_count - 3; // far past anything the tree uses
        set_block_bit(&fs, free, true);
        let repairs = recovered(&io, fs);
        assert_eq!(repairs.leaked_blocks.first, [free]);
        assert!(repairs.lost_found.is_empty() && repairs.freed_inodes.is_empty());
    }
}

#[test]
fn an_unreachable_file_with_data_goes_to_lost_found() {
    for block_size in BLOCK_SIZES {
        let (io, fs) = tree(block_size);
        let file = drop_name(&fs, "/a/sub", "two");
        let repairs = recovered(&io, fs);
        assert_eq!(repairs.lost_found.first, [file]);
        let fs = open(&io);
        let saved = fs.read_file(&std::format!("/lost+found/#{file}")).unwrap();
        assert_eq!(saved, pattern(70_000));
        assert!(fs.lookup("/a/sub/two").is_err());
    }
}

#[test]
fn an_unreachable_directory_goes_to_lost_found_whole() {
    let (io, fs) = tree(1024);
    let dir = drop_name(&fs, "/", "a");
    let repairs = recovered(&io, fs);
    assert_eq!(repairs.lost_found.first, [dir], "only the top is linked");
    let fs = open(&io);
    let top = std::format!("/lost+found/#{dir}");
    assert_eq!(
        fs.read_file(&std::format!("{top}/one")).unwrap(),
        pattern(5000)
    );
    assert_eq!(
        fs.read_file(&std::format!("{top}/sub/two")).unwrap(),
        pattern(70_000)
    );
    assert_eq!(fs.find_entry(dir, "..").unwrap().0, ino(&fs, "/lost+found"));
    assert_eq!(fs.link_count("/").unwrap(), 4, "root: ., .., lost+found, b");
}

#[test]
fn empty_unreachable_inodes_are_freed() {
    let (io, fs) = tree(2048);
    fs.create("/empty", 0o644, Owner::ROOT).unwrap();
    fs.mkdir("/b/hollow", 0o755, Owner::ROOT).unwrap();
    let file = drop_name(&fs, "/", "empty");
    let dir = drop_name(&fs, "/b", "hollow");
    let repairs = recovered(&io, fs);
    assert_eq!(repairs.freed_inodes.first, [file, dir]);
    assert_eq!(repairs.leaked_blocks.count, 1, "the directory's block");
    assert!(repairs.lost_found.is_empty());
    let fs = open(&io);
    assert_eq!(
        fs.link_count("/b").unwrap(),
        2,
        "the parent lost hollow's `..`"
    );
}

/// A delete whose inode landed (no links, a deletion time) but whose name
/// removal and frees did not: the name goes, then the inode and its blocks.
#[test]
fn an_entry_naming_a_deleted_inode_is_removed() {
    let (io, fs) = tree(4096);
    let file = ino(&fs, "/a/one");
    edit_inode(&fs, file, |inode| {
        put16(inode, INO_LINKS, 0);
        put32(inode, INO_DTIME, 5);
    });
    let dir = ino(&fs, "/a");
    let repairs = recovered(&io, fs);
    assert_eq!(repairs.dead_entries.first, [(dir, file)]);
    assert_eq!(repairs.freed_inodes.first, [file]);
    assert_eq!(repairs.leaked_blocks.count, 2, "5000 bytes in 4 KiB blocks");
    assert!(open(&io).lookup("/a/one").is_err());
}

/// A create whose entry landed ahead of its inode: the inode is allocated
/// but never written (it reads as an unsupported type).
#[test]
fn an_entry_naming_an_uninitialised_inode_is_removed() {
    let (io, fs) = tree(1024);
    let ghost = fs.alloc_inode(false).unwrap();
    let mut root = fs.read_inode(ROOT_INO).unwrap();
    fs.add_entry(ROOT_INO, &mut root, "ghost", ghost, FT_REGULAR)
        .unwrap();
    let repairs = recovered(&io, fs);
    assert_eq!(repairs.dead_entries.first, [(ROOT_INO, ghost)]);
    assert_eq!(repairs.freed_inodes.first, [ghost]);
}

#[test]
fn link_counts_follow_the_entries() {
    let (io, fs) = tree(2048);
    // A rename cut short before its last step: links 2, one name.
    let high = ino(&fs, "/a/one");
    edit_inode(&fs, high, |inode| put16(inode, INO_LINKS, 2));
    // A second name whose link count never landed: raised, never freed.
    let low = ino(&fs, "/b/three");
    let a = ino(&fs, "/a");
    let mut dir = fs.read_inode(a).unwrap();
    fs.add_entry(a, &mut dir, "three-again", low, FT_REGULAR)
        .unwrap();
    let repairs = recovered(&io, fs);
    assert_eq!(repairs.link_counts.first, [(high, 2, 1), (low, 1, 2)]);
    let fs = open(&io);
    assert_eq!(fs.read_file("/a/three-again").unwrap(), pattern(100));
    assert_eq!(fs.read_file("/b/three").unwrap(), pattern(100));
}

/// A directory rename cut short: both names, `..` at either parent. The
/// name `..` agrees with stays; with one name, `..` follows it.
#[test]
fn a_directory_keeps_the_name_its_dotdot_agrees_with() {
    for moved_dotdot in [false, true] {
        let (io, fs) = tree(1024);
        let sub = ino(&fs, "/a/sub");
        let (a, b) = (ino(&fs, "/a"), ino(&fs, "/b"));
        let mut dir = fs.read_inode(b).unwrap();
        fs.add_entry(b, &mut dir, "sub", sub, FT_DIRECTORY).unwrap();
        if moved_dotdot {
            let mut inode = fs.read_inode(sub).unwrap();
            fs.set_dotdot(sub, &mut inode, b).unwrap();
        }
        let repairs = recovered(&io, fs);
        let (kept, dropped) = if moved_dotdot {
            ("/b/sub", a)
        } else {
            ("/a/sub", b)
        };
        assert_eq!(repairs.extra_dir_names.first, [(dropped, sub)]);
        let fs = open(&io);
        assert_eq!(
            fs.read_file(&std::format!("{kept}/two")).unwrap(),
            pattern(70_000)
        );
    }
    let (io, fs) = tree(4096);
    let sub = ino(&fs, "/a/sub");
    let mut inode = fs.read_inode(sub).unwrap();
    fs.set_dotdot(sub, &mut inode, ROOT_INO).unwrap();
    let repairs = recovered(&io, fs);
    let a = open(&io).resolve("/a").unwrap();
    assert_eq!(repairs.dotdot.first, [(sub, a)]);
}

#[test]
fn sizes_block_counts_and_counters_are_recomputed() {
    let (io, fs) = tree(1024);
    let dir = ino(&fs, "/b");
    edit_inode(&fs, dir, |inode| put32(inode, INO_SIZE, 3 * 1024));
    let file = ino(&fs, "/top");
    edit_inode(&fs, file, |inode| {
        let sectors = le32(inode, INO_BLOCKS) + 2;
        put32(inode, INO_BLOCKS, sectors);
    });
    let mut desc = fs.read_group(0).unwrap();
    desc.free_inodes -= 1;
    desc.used_dirs += 1;
    fs.write_group(0, &desc).unwrap();
    let mut raw = [0u8; 1024];
    fs.read_super_raw(&mut raw).unwrap();
    let free = le32(&raw, SB_FREE_BLOCKS) + 7;
    put32(&mut raw, SB_FREE_BLOCKS, free);
    fs.write_super_raw(&raw).unwrap();
    let repairs = recovered(&io, fs);
    assert_eq!(repairs.dir_sizes.first, [dir]);
    assert_eq!(repairs.block_counts.first, [file]);
    assert_eq!(repairs.group_counters.first, [0]);
    assert!(repairs.super_counters);
    let summary = std::format!("{repairs}");
    assert!(
        summary.contains("1 directory size") && summary.contains("superblock"),
        "{summary}"
    );
}

#[test]
fn a_consistent_volume_is_left_untouched() {
    let (io, fs) = tree(4096);
    drop(fs);
    let before = io.snapshot();
    assert_eq!(open(&io).repair(), Ok(RepairReport::default()));
    assert!(io.snapshot() == before);
}

#[test]
fn a_reachable_block_marked_free_is_refused() {
    let (io, fs) = tree(1024);
    let block = fs.mapped_block("/a/one", 0).unwrap();
    set_block_bit(&fs, block, false);
    set_block_bit(&fs, fs.blocks_count - 3, true); // a leak beside it changes nothing
    refused(&io, fs, "marked free");
}

#[test]
fn a_doubly_claimed_block_is_refused() {
    let (io, fs) = tree(2048);
    let shared = fs.mapped_block("/a/one", 0).unwrap();
    edit_inode(&fs, ino(&fs, "/b/three"), |inode| {
        put32(inode, INO_BLOCK, shared)
    });
    refused(&io, fs, "claimed twice");
}

#[test]
fn a_pointer_into_metadata_or_out_of_range_is_refused() {
    let (io, fs) = tree(1024);
    let table = fs.read_group(0).unwrap().inode_table;
    edit_inode(&fs, ino(&fs, "/top"), |inode| {
        put32(inode, INO_BLOCK, table)
    });
    refused(&io, fs, "claimed twice or is metadata");
    let (io, fs) = tree(1024);
    let past = fs.blocks_count + 5;
    edit_inode(&fs, ino(&fs, "/top"), |inode| {
        put32(inode, INO_BLOCK + 4, past)
    });
    refused(&io, fs, "out of range");
}

#[test]
fn a_garbled_directory_or_a_hole_is_refused() {
    let (io, fs) = tree(4096);
    let block = fs
        .dir_blocks(&fs.read_inode(ino(&fs, "/b")).unwrap())
        .unwrap()[0];
    let mut data = std::vec![0u8; 4096];
    fs.read_block(u64::from(block), &mut data).unwrap();
    put16(&mut data, 12 + DE_REC_LEN, 6); // the `..` record
    fs.write_block(u64::from(block), &data).unwrap();
    refused(&io, fs, "bad directory record");
    let (io, fs) = tree(1024);
    let dir = ino(&fs, "/a");
    edit_inode(&fs, dir, |inode| {
        let first = le32(inode, INO_BLOCK);
        put32(inode, INO_BLOCK, 0);
        put32(inode, INO_BLOCK + 4, first);
        put32(inode, INO_SIZE, 2 * 1024);
    });
    refused(&io, fs, "hole");
}

#[test]
fn an_entry_naming_a_free_inode_is_refused() {
    let (io, fs) = tree(1024);
    let file = ino(&fs, "/b/three");
    let index = file - 1;
    let desc = fs.read_group(index / fs.inodes_per_group).unwrap();
    let mut bitmap = std::vec![0u8; 1024];
    fs.read_block(u64::from(desc.inode_bitmap), &mut bitmap)
        .unwrap();
    Ext2::bitmap_clear(&mut bitmap, index % fs.inodes_per_group).unwrap();
    fs.write_block(u64::from(desc.inode_bitmap), &bitmap)
        .unwrap();
    refused(&io, fs, "marked free");
}
