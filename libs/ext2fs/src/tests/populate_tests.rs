//! The populator: `mkdir_p`, `write_file`, `read_file` and `remove_tree`.

use super::*;
use crate::{Ext2Error, FileKind, S_IFDIR, S_IFREG};

#[test]
fn mkdir_p_creates_missing_components_only() {
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    let meta = fs.mkdir_p("/data/home/alice", 0o755, 1000, 1000).unwrap();
    assert_eq!(
        (meta.mode, meta.uid, meta.gid),
        (S_IFDIR | 0o755, 1000, 1000)
    );
    // Every new component takes the given owner; existing ones are left alone.
    fs.setattr(
        "/data",
        &crate::AttrChange {
            mode: Some(0o700),
            ..Default::default()
        },
    )
    .unwrap();
    fs.mkdir_p("/data/tmp", 0o1777, 0, 0).unwrap();
    assert_eq!(fs.lookup("/data").unwrap().mode, S_IFDIR | 0o700);
    assert_eq!(fs.lookup("/data/tmp").unwrap().mode, S_IFDIR | 0o1777);
    assert_eq!(
        fs.mkdir_p("/data/home/alice", 0o700, 5, 5).unwrap().mode,
        S_IFDIR | 0o755
    );
    assert_eq!(fs.mkdir_p("/", 0o755, 0, 0).unwrap().ino, 2);
    assert_eq!(
        fs.mkdir_p("//data//home/", 0o755, 0, 0).unwrap().kind,
        FileKind::Dir
    );
    fs.write_file("/data/file", b"x", 0o644, 0, 0, 1).unwrap();
    assert_eq!(
        fs.mkdir_p("/data/file/sub", 0o755, 0, 0),
        Err(Ext2Error::NotDir)
    );
    assert_eq!(
        fs.mkdir_p("/data/file", 0o755, 0, 0),
        Err(Ext2Error::NotDir)
    );
    assert_clean(&io);
}

#[test]
fn write_file_sets_attributes_and_is_reproducible() {
    let make = || {
        let (io, fs) = fresh(2 * 1024 * 1024, 4096);
        fs.mkdir_p("/system/bin", 0o755, 0, 0).unwrap();
        fs.write_file(
            "/system/bin/tool",
            &[0x7F, b'E', b'L', b'F'],
            0o755,
            0,
            0,
            1_600_000_000,
        )
        .unwrap();
        fs.write_file("/PASSWD", b"root:x:0:0\n", 0o644, 0, 0, 1_600_000_000)
            .unwrap();
        fs.flush().unwrap();
        io.snapshot()
    };
    assert_eq!(make(), make(), "same input, same image");
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    let meta = fs.write_file("/f", b"abc", 0o640, 12, 34, 777).unwrap();
    assert_eq!(
        (meta.mode, meta.uid, meta.gid, meta.size),
        (S_IFREG | 0o640, 12, 34, 3)
    );
    assert_eq!(
        (meta.times.atime, meta.times.mtime, meta.times.ctime),
        (777, 777, 777)
    );
    assert_clean(&io);
}

#[test]
fn write_file_replaces_in_place_and_frees_the_old_blocks() {
    let (io, fs) = fresh(2 * 1024 * 1024, 1024);
    let first = fs
        .write_file("/f", &std::vec![1u8; 50_000], 0o644, 0, 0, 1)
        .unwrap();
    let blocks = fs.free_blocks().unwrap();
    let second = fs.write_file("/f", b"short", 0o600, 5, 6, 2).unwrap();
    assert_eq!(first.ino, second.ino, "replaced, not recreated");
    assert_eq!(
        (second.size, second.mode & 0o7777, second.uid),
        (5, 0o600, 5)
    );
    assert!(fs.free_blocks().unwrap() > blocks + 40);
    assert_eq!(fs.read_file("/f").unwrap(), b"short");
    fs.write_file("/f", b"", 0o600, 5, 6, 3).unwrap();
    assert_eq!(fs.read_file("/f").unwrap(), b"");
    assert_clean(&io);
}

#[test]
fn write_file_errors() {
    let (_, fs) = fresh(1024 * 1024, 1024);
    fs.mkdir("/d", 0o755, crate::Owner::ROOT).unwrap();
    assert_eq!(
        fs.write_file("/d", b"x", 0o644, 0, 0, 0),
        Err(Ext2Error::IsDir)
    );
    assert_eq!(
        fs.write_file("/nodir/f", b"x", 0o644, 0, 0, 0),
        Err(Ext2Error::NotFound)
    );
    assert_eq!(
        fs.write_file("/f", b"x", 0o644, 70_000, 0, 0),
        Err(Ext2Error::Invalid)
    );
    let too_big = std::vec![1u8; 2 * 1024 * 1024];
    assert_eq!(
        fs.write_file("/big", &too_big, 0o644, 0, 0, 0),
        Err(Ext2Error::NoSpace)
    );
    assert_eq!(fs.read_file("/d"), Err(Ext2Error::IsDir));
    assert_eq!(fs.read_file("/nope"), Err(Ext2Error::NotFound));
}

#[test]
fn remove_tree_deletes_everything_below_a_path() {
    let (io, fs) = fresh(4 * 1024 * 1024, 1024);
    let (blocks, inodes) = (fs.free_blocks().unwrap(), fs.free_inodes().unwrap());
    fs.mkdir_p("/t/a/b/c", 0o755, 0, 0).unwrap();
    fs.mkdir_p("/t/z", 0o755, 0, 0).unwrap();
    for n in 0..30 {
        fs.write_file(
            &std::format!("/t/a/b/f{n}"),
            &std::vec![n as u8; 3000],
            0o644,
            0,
            0,
            1,
        )
        .unwrap();
        fs.write_file(&std::format!("/t/z/g{n}"), b"g", 0o644, 0, 0, 1)
            .unwrap();
    }
    fs.write_file("/keep", b"keep", 0o644, 0, 0, 1).unwrap();
    fs.remove_tree("/t/").unwrap();
    assert_eq!(fs.lookup("/t"), Err(Ext2Error::NotFound));
    assert_eq!(fs.read_file("/keep").unwrap(), b"keep");
    fs.unlink("/keep").unwrap();
    assert_eq!(
        (fs.free_blocks().unwrap(), fs.free_inodes().unwrap()),
        (blocks, inodes)
    );
    assert_eq!(fs.link_count("/").unwrap(), 3);
    // A single file works too; the root and missing paths are refused.
    fs.write_file("/one", b"1", 0o644, 0, 0, 1).unwrap();
    fs.remove_tree("/one").unwrap();
    assert_eq!(fs.remove_tree("/"), Err(Ext2Error::Invalid));
    assert_eq!(fs.remove_tree(""), Err(Ext2Error::Invalid));
    assert_eq!(fs.remove_tree("/missing"), Err(Ext2Error::NotFound));
    assert_clean(&io);
}

#[test]
fn remove_tree_refuses_deep_chains() {
    let (_, fs) = fresh(4 * 1024 * 1024, 1024);
    let deep: String = (0..70).map(|n| std::format!("/d{n}")).collect();
    fs.mkdir_p(&deep, 0o755, 0, 0).unwrap();
    assert_eq!(fs.remove_tree("/d0"), Err(Ext2Error::Invalid));
    assert!(
        fs.lookup("/d0").is_ok(),
        "nothing above the limit was half-removed"
    );
}
