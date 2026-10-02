//! The F4 layout (docs/filesystem-plan.md, issue #508): an F3-built image
//! updated in place by the F4 build. Nothing new is seeded under `/data`, the
//! old `/data/home/<user>` and `/data/tmp` go only when empty (user files
//! there survive until F7), and the service directories converge to their new
//! modes.

use ext2fs::memio::MemIo;
use ext2fs::{Ext2, Geometry};

use crate::layout_tests::pre_f4_layout;
use crate::os_image::{write_volume, OsFile, Source};
use crate::os_layout::{dirs, parse_passwd};

const STAMP: i64 = 1_700_000_000;
const PASSWD: &str = "root:0:0:toor:/root:sh\nalice:1000:1000:lazy:/home/alice:sh\n";

fn volume() -> (MemIo, Ext2) {
    let io = MemIo::new(8 << 20);
    let geometry = Geometry {
        block_size: 4096,
        blocks_count: (8 << 20) / 4096,
        bytes_per_inode: 16 * 1024,
    };
    ext2fs::format(&io, geometry, "lazyos-root", [4; 16], STAMP).unwrap();
    let volume = Ext2::open(Box::new(io.clone()), || STAMP).unwrap();
    (io, volume)
}

fn files() -> Vec<OsFile> {
    vec![OsFile {
        path: fhs::etc::PASSWD.into(),
        source: Source::Bytes(PASSWD.as_bytes().to_vec()),
        mode: 0o644,
    }]
}

fn mode_owner(volume: &Ext2, path: &str) -> (u16, u32, u32) {
    let meta = volume.lookup(path).unwrap();
    (meta.mode & 0o7777, meta.uid, meta.gid)
}

#[test]
fn an_update_keeps_non_empty_data_dirs_and_drops_empty_ones() {
    let (io, volume) = volume();
    let old = write_volume(&volume, None, &pre_f4_layout(), &files(), STAMP).unwrap();
    assert!(old.entries.contains_key("/data/home/alice"));
    // The user's file in the old home, and settings and an app on /data the
    // way a data-disk-era image kept them.
    volume
        .write_file(
            "/data/home/alice/note.txt",
            b"mine",
            0o644,
            1000,
            1000,
            STAMP,
        )
        .unwrap();
    volume.mkdir_p("/data/confd", 0o700, 0, 0).unwrap();
    volume
        .write_file("/data/confd/store", b"settings", 0o600, 0, 0, STAMP)
        .unwrap();

    let new = write_volume(
        &volume,
        Some(&old),
        &dirs(&parse_passwd(PASSWD)),
        &files(),
        STAMP,
    )
    .unwrap();
    assert!(new.entries.keys().all(|path| !path.starts_with("/data/")));

    // Non-empty: kept with its contents. Empty: removed. Never seeded again.
    assert_eq!(
        volume.read_file("/data/home/alice/note.txt").unwrap(),
        b"mine"
    );
    assert_eq!(volume.read_file("/data/confd/store").unwrap(), b"settings");
    assert!(
        volume.lookup("/data/tmp").is_err(),
        "the empty /data/tmp stayed"
    );
    assert!(volume.lookup("/data").is_ok());

    // The services' directories converge to the F4 table.
    assert_eq!(mode_owner(&volume, "/conf"), (0o700, 0, 0));
    assert_eq!(mode_owner(&volume, "/conf/svc"), (0o700, 0, 0));
    assert_eq!(mode_owner(&volume, "/logs"), (0o750, 0, 0));
    assert_eq!(mode_owner(&volume, "/apps"), (0o755, 0, 0));
    assert_eq!(mode_owner(&volume, "/docs/apps"), (0o755, 0, 0));
    assert_eq!(mode_owner(&volume, "/home/alice"), (0o700, 1000, 1000));
    volume.flush().unwrap();
    drop(volume);
    let problems = ext2fs::check::fsck(&io.snapshot());
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn a_second_update_changes_nothing_and_keeps_user_state() {
    let (_io, volume) = volume();
    let layout = dirs(&parse_passwd(PASSWD));
    let first = write_volume(&volume, None, &layout, &files(), STAMP).unwrap();
    volume
        .write_file("/conf/store", b"k=v", 0o600, 0, 0, STAMP)
        .unwrap();
    volume
        .write_file("/logs/pkg.log", b"1 00 00 00\n", 0o644, 0, 0, STAMP)
        .unwrap();
    volume
        .write_file("/home/alice/notes.txt", b"mine", 0o600, 1000, 1000, STAMP)
        .unwrap();
    let second = write_volume(&volume, Some(&first), &layout, &files(), STAMP).unwrap();
    assert_eq!(first, second);
    for (path, bytes) in [
        ("/conf/store", &b"k=v"[..]),
        ("/logs/pkg.log", b"1 00 00 00\n"),
        ("/home/alice/notes.txt", b"mine"),
    ] {
        assert_eq!(volume.read_file(path).unwrap(), bytes, "{path}");
    }
}
