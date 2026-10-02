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

/// A volume the checker finds inconsistent is updated but stays flagged
/// unclean, and nothing of the user's is removed.
#[test]
fn an_update_leaves_an_inconsistent_volume_flagged() {
    let dir = Scratch::new();
    build(&dir, &first_files(), &settings()).unwrap();
    let volume = open_rw(&dir.image());
    volume
        .write_file("/data/mine.txt", b"user data", 0o644, 0, 0, STAMP)
        .unwrap();
    drop(volume);
    // Lose one block from the superblock's free counter.
    let mut bytes = std::fs::read(dir.image()).unwrap();
    let counter = (OS_START_LBA * SECTOR) as usize + 1024 + 0x0C;
    bytes[counter] = bytes[counter].wrapping_sub(1);
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
