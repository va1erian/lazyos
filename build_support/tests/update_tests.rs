//! What an in-place update does when it cannot finish (a full volume, a
//! missing input), and what it leaves alone when it can.
//!
//! The manifest is written last, so a build that died halfway used to leave
//! files nobody listed, which no later build could free: one failed update made
//! the next fail too. `os_update` closes that.

use ext2fs::{AttrChange, BlockIo};

use crate::image_tests::{
    assert_fsck_clean, build, first_files, open_rw, settings, Scratch, STAMP,
};
use crate::os_disk::{FileIo, SECTOR};
use crate::os_image::{validate, OsFile, Placement, Source};

const MIB: usize = 1 << 20;

fn file(path: &str, bytes: Vec<u8>) -> OsFile {
    OsFile {
        path: path.into(),
        source: Source::Bytes(bytes),
        mode: 0o644,
        placement: Placement::ROOT,
    }
}

#[test]
fn an_update_that_dies_halfway_leaves_its_files_in_the_manifest() {
    let dir = Scratch::new();
    build(&dir, &first_files(), &settings()).unwrap();

    let mut next = first_files();
    next.push(file("/system/share/a.bin", vec![1u8; 20_000]));
    next.push(OsFile {
        path: "/system/share/z-missing.bin".into(),
        source: Source::Path(dir.image().with_file_name("does-not-exist")),
        mode: 0o644,
        placement: Placement::ROOT,
    });
    let error = build(&dir, &next, &settings()).unwrap_err();
    assert!(error.contains("does-not-exist"), "{error}");

    // The file written before the failure is listed, beside everything the
    // earlier build placed.
    let (_, manifest, _) = validate(&dir.image()).unwrap();
    assert!(manifest.entries.contains_key("/system/share/a.bin"));
    assert!(manifest.entries.contains_key("/SUPER.ELF"));

    // A later build without it frees it.
    build(&dir, &first_files(), &settings()).unwrap();
    let volume = open_rw(&dir.image());
    assert!(volume.lookup("/system/share/a.bin").is_err());
    assert!(volume.lookup("/SUPER.ELF").is_ok());
    assert_fsck_clean(&dir.image());
}

#[test]
fn a_full_volume_loses_the_leftovers_of_an_interrupted_build_not_the_state() {
    let dir = Scratch::new();
    build(&dir, &first_files(), &settings()).unwrap();
    {
        // What an older build left: a big file no manifest lists in a
        // build-owned tree, and the state a running system keeps.
        let volume = open_rw(&dir.image());
        volume.mkdir_p("/system/share/old", 0o755, 0, 0).unwrap();
        volume
            .write_file(
                "/system/share/old/data.bin",
                &vec![7u8; 6 * MIB],
                0o644,
                0,
                0,
                STAMP,
            )
            .unwrap();
        volume
            .write_file("/apps/state.bin", &vec![3u8; 8192], 0o644, 0, 0, STAMP)
            .unwrap();
        volume.flush().unwrap();
    }

    // Four more MiB than the leftover leaves room for.
    let mut next = first_files();
    next.push(file("/system/share/new.bin", vec![9u8; 4 * MIB]));
    build(&dir, &next, &settings()).unwrap();

    let volume = open_rw(&dir.image());
    assert!(volume.lookup("/system/share/old/data.bin").is_err());
    assert!(
        volume.lookup("/system/share/old").is_err(),
        "the directory it emptied goes too"
    );
    assert_eq!(
        volume.read_file("/apps/state.bin").unwrap(),
        vec![3u8; 8192]
    );
    assert_eq!(
        volume.read_file("/system/share/new.bin").unwrap(),
        vec![9u8; 4 * MIB]
    );
    assert_eq!(volume.read_file("/SUPER.ELF").unwrap(), b"super v1");
    assert_fsck_clean(&dir.image());
}

#[test]
fn state_outside_the_build_trees_is_never_reclaimed() {
    let dir = Scratch::new();
    build(&dir, &first_files(), &settings()).unwrap();
    {
        let volume = open_rw(&dir.image());
        volume
            .write_file("/apps/big.bin", &vec![5u8; 6 * MIB], 0o644, 0, 0, STAMP)
            .unwrap();
        volume.flush().unwrap();
    }
    let mut next = first_files();
    next.push(file("/system/share/new.bin", vec![9u8; 4 * MIB]));
    let error = build(&dir, &next, &settings()).unwrap_err();
    assert!(error.contains("OS volume is full"), "{error}");
    assert!(error.contains("LAZYOS_OS_SIZE"), "{error}");
    assert!(error.contains("LAZYOS_RESET_OS"), "{error}");
    assert_eq!(
        open_rw(&dir.image()).read_file("/apps/big.bin").unwrap(),
        vec![5u8; 6 * MIB],
        "what the user keeps survives a refusal"
    );
}

#[test]
fn a_new_volume_that_is_too_small_says_how_big_to_make_it() {
    let dir = Scratch::new();
    let mut files = first_files();
    files.push(file("/system/share/huge.bin", vec![1u8; 12 * MIB]));
    let error = build(&dir, &files, &settings()).unwrap_err();
    assert!(error.contains("OS volume is full"), "{error}");
    assert!(error.contains("LAZYOS_OS_SIZE"), "{error}");
    assert!(!dir.image().exists(), "no half-made image is left");
}

#[test]
fn a_file_that_is_already_there_is_not_written_again() {
    let dir = Scratch::new();
    build(&dir, &first_files(), &settings()).unwrap();
    {
        // Age two files; an update that rewrote them would stamp them again.
        let volume = open_rw(&dir.image());
        let old = AttrChange {
            mtime: Some(1),
            ..AttrChange::default()
        };
        volume.setattr("/SUPER.ELF", &old).unwrap();
        volume.setattr("/PASSWD", &old).unwrap();
        volume.flush().unwrap();
    }
    let mut next = first_files();
    next[1] = file("/PASSWD", b"admin:0:0\nuser:1:1\n".to_vec());
    build(&dir, &next, &settings()).unwrap();

    let volume = open_rw(&dir.image());
    assert_eq!(
        volume.lookup("/SUPER.ELF").unwrap().times.mtime,
        1,
        "same bytes"
    );
    assert!(
        volume.lookup("/PASSWD").unwrap().times.mtime > 1,
        "new bytes"
    );
    assert_eq!(
        volume.read_file("/PASSWD").unwrap(),
        b"admin:0:0\nuser:1:1\n"
    );
    assert_fsck_clean(&dir.image());
}

#[test]
fn a_file_that_is_already_there_still_gets_the_mode_the_build_wants() {
    let dir = Scratch::new();
    build(&dir, &first_files(), &settings()).unwrap();
    {
        let volume = open_rw(&dir.image());
        let wrong = AttrChange {
            mode: Some(0o600),
            ..AttrChange::default()
        };
        volume.setattr("/SUPER.ELF", &wrong).unwrap();
        volume.flush().unwrap();
    }
    build(&dir, &first_files(), &settings()).unwrap();
    let meta = open_rw(&dir.image()).lookup("/SUPER.ELF").unwrap();
    assert_eq!(meta.mode & 0o7777, 0o755);
}

#[test]
fn a_volatile_device_reads_and_writes_runs_like_a_durable_one() {
    let dir = Scratch::new();
    let path = dir.image().with_file_name("raw.bin");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    file.set_len(64 * SECTOR).unwrap();
    let io = FileIo::volatile(file, 0, 64, true);
    let (a, b, c) = (vec![1u8; 512], vec![2u8; 1024], vec![3u8; 512]);
    io.write_sectors_vectored(4, &[&a, &b, &c]).unwrap();
    io.flush().unwrap();

    let mut whole = vec![0u8; 2048];
    io.read_sectors(4, &mut whole).unwrap();
    assert_eq!(&whole[..512], &a[..]);
    assert_eq!(&whole[512..1536], &b[..]);
    assert_eq!(&whole[1536..], &c[..]);

    let (mut x, mut y) = (vec![0u8; 1024], vec![0u8; 1024]);
    io.read_sectors_vectored(5, &mut [&mut x[..], &mut y[..]])
        .unwrap();
    assert_eq!(x, b);
    assert_eq!(&y[..512], &c[..]);
    assert!(
        io.write_sectors_vectored(63, &[&a, &a]).is_err(),
        "past the end"
    );
}

#[test]
fn a_file_that_becomes_a_directory_is_replaced_and_the_manifest_follows() {
    let dir = Scratch::new();
    let mut first = first_files();
    first.push(file("/system/share/thing", b"a file".to_vec()));
    build(&dir, &first, &settings()).unwrap();

    let mut next = first_files();
    next.push(file("/system/share/thing/inner.txt", b"inside".to_vec()));
    build(&dir, &next, &settings()).unwrap();

    let volume = open_rw(&dir.image());
    assert_eq!(
        volume.read_file("/system/share/thing/inner.txt").unwrap(),
        b"inside"
    );
    let (_, manifest, _) = validate(&dir.image()).unwrap();
    assert!(manifest
        .entries
        .contains_key("/system/share/thing/inner.txt"));
    assert_fsck_clean(&dir.image());
}

#[test]
fn a_seed_in_a_build_tree_survives_a_reclaim_with_what_the_system_made_of_it() {
    let dir = Scratch::new();
    let seed = |bytes: &[u8]| OsFile {
        path: "/system/share/seed.conf".into(),
        source: Source::Bytes(bytes.to_vec()),
        mode: 0o644,
        placement: Placement::seed(0, 0),
    };
    let mut first = first_files();
    first.push(seed(b"shipped"));
    build(&dir, &first, &settings()).unwrap();
    {
        // The running system changed the seed; an older build left a big file.
        let volume = open_rw(&dir.image());
        volume
            .write_file("/system/share/seed.conf", b"changed", 0o644, 0, 0, STAMP)
            .unwrap();
        volume
            .write_file(
                "/system/share/leftover.bin",
                &vec![7u8; 6 * MIB],
                0o644,
                0,
                0,
                STAMP,
            )
            .unwrap();
        volume.flush().unwrap();
    }
    let mut next = first_files();
    next.push(seed(b"shipped"));
    next.push(file("/system/share/new.bin", vec![9u8; 4 * MIB]));
    build(&dir, &next, &settings()).unwrap();

    let volume = open_rw(&dir.image());
    assert!(volume.lookup("/system/share/leftover.bin").is_err());
    assert_eq!(
        volume.read_file("/system/share/seed.conf").unwrap(),
        b"changed"
    );
    assert_fsck_clean(&dir.image());
}

#[test]
fn a_file_changed_on_the_volume_to_the_same_length_is_written_back() {
    let dir = Scratch::new();
    build(&dir, &first_files(), &settings()).unwrap();
    {
        // "super v1" -> "super v9": the size alone would say it is current.
        let volume = open_rw(&dir.image());
        volume
            .write_file("/SUPER.ELF", b"super v9", 0o755, 0, 0, STAMP)
            .unwrap();
        volume.flush().unwrap();
    }
    build(&dir, &first_files(), &settings()).unwrap();
    assert_eq!(
        open_rw(&dir.image()).read_file("/SUPER.ELF").unwrap(),
        b"super v1"
    );
}

#[test]
fn the_manifest_is_never_left_empty_or_beside_a_second_copy() {
    let dir = Scratch::new();
    build(&dir, &first_files(), &settings()).unwrap();
    let volume = open_rw(&dir.image());
    assert!(volume.lookup("/system/.image-manifest").is_ok());
    assert!(
        volume.lookup("/system/.image-manifest.new").is_err(),
        "the new text was renamed over it"
    );
    let (_, manifest, _) = validate(&dir.image()).unwrap();
    assert!(manifest.entries.contains_key("/SUPER.ELF"));
}
