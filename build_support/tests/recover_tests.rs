//! An in-place update re-certifies an OS volume that stopped uncleanly
//! (#512), now through the build's block cache: the update's `recover`
//! reclaims orphans, commits, runs the independent checker and only then lets
//! the closing flush mark the volume clean.

use std::path::Path;

use crate::image_tests::{
    assert_fsck_clean, build, first_files, open_rw, partition_bytes, settings, Scratch, STAMP,
};
use crate::os_disk::{OS_START_LBA, SECTOR};

/// `s_state` of the image's OS volume.
fn volume_state(image: &Path) -> u8 {
    partition_bytes(image)[1024 + 0x3A]
}

/// An unclean stop (a change with no flush) is checked by the next update,
/// which marks the volume clean again and keeps the user's file; without the
/// check the kernel would restore "unclean" at every shutdown, for good.
#[test]
fn an_update_recovers_a_volume_that_stopped_uncleanly() {
    let dir = Scratch::new();
    build(&dir, &first_files(), &settings()).unwrap();
    let volume = open_rw(&dir.image());
    volume
        .write_file("/data/mine.txt", b"user data", 0o644, 0, 0, STAMP)
        .unwrap();
    volume
        .write_file("/data/.unlinked-4", b"parked", 0o644, 0, 0, STAMP)
        .unwrap();
    drop(volume); // never flushed: the window was closed
    assert_eq!(volume_state(&dir.image()) & 1, 0);

    build(&dir, &first_files(), &settings()).unwrap();
    assert_eq!(
        volume_state(&dir.image()),
        1,
        "the update left the volume unclean"
    );
    let volume = open_rw(&dir.image());
    assert!(volume.was_clean_at_mount());
    assert_eq!(volume.read_file("/data/mine.txt").unwrap(), b"user data");
    assert!(
        volume.lookup("/data/.unlinked-4").is_err(),
        "the orphan survived"
    );
    drop(volume);
    assert_fsck_clean(&dir.image());
}

/// The damage `pkg_install.json` left when QEMU was killed mid-session: a
/// link count above its entries (a rename cut short), and a deleted file
/// whose deferred frees never landed (an unreachable inode and block, and
/// counters that disagree with the bitmaps). The update repairs it and
/// re-certifies the volume, keeping every file.
#[test]
fn an_update_repairs_what_a_killed_session_left() {
    let dir = Scratch::new();
    build(&dir, &first_files(), &settings()).unwrap();
    let volume = open_rw(&dir.image());
    volume
        .write_file("/data/mine.txt", b"user data", 0o644, 0, 0, STAMP)
        .unwrap();
    volume
        .write_file("/data/gone.txt", &[7u8; 3000], 0o644, 0, 0, STAMP)
        .unwrap();
    let mine = volume.lookup("/data/mine.txt").unwrap().ino as u32;
    let gone = volume.lookup("/data/gone.txt").unwrap().ino as u32;
    let block = volume.mapped_block("/data/gone.txt", 0).unwrap();
    volume.unlink("/data/gone.txt").unwrap();
    drop(volume); // never flushed
    let mut bytes = std::fs::read(dir.image()).unwrap();
    let volume_bytes = &mut bytes[(OS_START_LBA * SECTOR) as usize..];
    let at = Ext2Bytes(volume_bytes).inode(mine) + 0x1A;
    volume_bytes[at] = 2; // i_links_count: two links, one name
    Ext2Bytes(volume_bytes).mark_used(gone, block);
    std::fs::write(dir.image(), &bytes).unwrap();
    assert!(!ext2fs::check::fsck(&partition_bytes(&dir.image())).is_empty());

    build(&dir, &first_files(), &settings()).unwrap();
    assert_eq!(volume_state(&dir.image()), 1, "the damage was not repaired");
    assert_fsck_clean(&dir.image());
    let volume = open_rw(&dir.image());
    assert_eq!(volume.read_file("/data/mine.txt").unwrap(), b"user data");
    assert_eq!(volume.link_count("/data/mine.txt").unwrap(), 1);
    assert!(volume.lookup("/data/gone.txt").is_err());
}

/// A volume with damage no crash leaves (here one block claimed by two
/// files) is updated but stays flagged unclean, and nothing of the user's is
/// removed.
#[test]
fn an_update_leaves_an_inconsistent_volume_flagged() {
    let dir = Scratch::new();
    build(&dir, &first_files(), &settings()).unwrap();
    let volume = open_rw(&dir.image());
    volume
        .write_file("/data/mine.txt", b"user data", 0o644, 0, 0, STAMP)
        .unwrap();
    volume
        .write_file("/data/other.txt", b"other data", 0o644, 0, 0, STAMP)
        .unwrap();
    let block = volume.mapped_block("/data/mine.txt", 0).unwrap();
    let other = volume.lookup("/data/other.txt").unwrap().ino as u32;
    drop(volume);
    let mut bytes = std::fs::read(dir.image()).unwrap();
    let volume_bytes = &mut bytes[(OS_START_LBA * SECTOR) as usize..];
    let at = Ext2Bytes(volume_bytes).inode(other) + 0x28; // i_block[0]
    volume_bytes[at..at + 4].copy_from_slice(&block.to_le_bytes());
    std::fs::write(dir.image(), &bytes).unwrap();

    build(&dir, &first_files(), &settings()).unwrap();
    assert_eq!(
        volume_state(&dir.image()) & 1,
        0,
        "an inconsistent volume was blessed"
    );
    assert_eq!(
        open_rw(&dir.image()).read_file("/data/mine.txt").unwrap(),
        b"user data"
    );
}

/// Just enough of the on-disk ext2 layout to damage a volume by hand.
struct Ext2Bytes<'a>(&'a mut [u8]);

impl Ext2Bytes<'_> {
    fn le32(&self, at: usize) -> usize {
        u32::from_le_bytes(self.0[at..at + 4].try_into().unwrap()) as usize
    }

    fn block_size(&self) -> usize {
        1024 << self.le32(1024 + 0x18)
    }

    /// Byte offset of group `group`'s descriptor.
    fn descriptor(&self, group: usize) -> usize {
        (self.le32(1024 + 0x14) + 1) * self.block_size() + group * 32
    }

    /// Byte offset of inode `ino`.
    fn inode(&self, ino: u32) -> usize {
        let per_group = self.le32(1024 + 0x28);
        let size = self.le32(1024 + 0x58) & 0xFFFF;
        let index = ino as usize - 1;
        let table = self.le32(self.descriptor(index / per_group) + 8);
        table * self.block_size() + (index % per_group) * size
    }

    fn set_used(&mut self, bitmap: usize, index: usize) {
        let at = bitmap * self.block_size() + index / 8;
        self.0[at] |= 1 << (index % 8);
    }

    /// Mark inode `ino` and `block` used again, as if their frees were lost.
    fn mark_used(&mut self, ino: u32, block: u32) {
        let per_group = self.le32(1024 + 0x28);
        let index = ino as usize - 1;
        let bitmap = self.le32(self.descriptor(index / per_group) + 4);
        self.set_used(bitmap, index % per_group);
        let index = block as usize - self.le32(1024 + 0x14);
        let per_group = self.le32(1024 + 0x20);
        let bitmap = self.le32(self.descriptor(index / per_group));
        self.set_used(bitmap, index % per_group);
    }
}
