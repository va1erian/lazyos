//! `Vfs::setattr` over ramfs (issue #345): the rule table in isolation, the
//! caches serving the changed attributes at once, and ramfs timestamps.

use super::*;
use crate::fs::vfs::{AttrRequest, Filesystem, SetAttr, Stamp};

const ALICE: Id = Id::new(1000, 100);
const BOB: Id = Id::new(2000, 200);

/// A regular file owned by `ALICE` with `mode`.
fn alice_meta(mode: u16) -> Meta {
    Meta {
        ino: 7,
        mode: vfs::S_IFREG | mode,
        uid: 1000,
        gid: 100,
        size: 0,
        kind: FileKind::File,
        times: vfs::Times::default(),
    }
}

fn owner(uid: Option<u32>, gid: Option<u32>) -> AttrRequest {
    AttrRequest::Owner { uid, gid }
}

fn stamps(atime: Option<Stamp>, mtime: Option<Stamp>) -> AttrRequest {
    AttrRequest::Times { atime, mtime }
}

/// The permission rules, one request at a time, with no filesystem at all.
pub fn setattr_rule_table() -> Result<(), String> {
    let file = alice_meta(0o644);
    let now = 77;
    let denied = Err(FsError::NotPermitted);
    let cases: &[(&str, Id, AttrRequest, Result<SetAttr, FsError>)] = &[
        (
            "owner chmod",
            ALICE,
            AttrRequest::Mode(0o100600),
            Ok(SetAttr {
                mode: Some(0o600), // type bits are not the caller's to set
                ctime: Some(now),
                ..SetAttr::default()
            }),
        ),
        ("stranger chmod", BOB, AttrRequest::Mode(0o600), denied),
        (
            "outsider setgid",
            Id::new(1000, 300),
            AttrRequest::Mode(0o2755),
            Ok(SetAttr {
                mode: Some(0o755),
                ctime: Some(now),
                ..SetAttr::default()
            }),
        ),
        ("owner gives away", ALICE, owner(Some(2000), None), denied),
        (
            "owner to a foreign group",
            ALICE,
            owner(None, Some(200)),
            denied,
        ),
        ("stranger chgrp", BOB, owner(None, Some(200)), denied),
        (
            "chown(-1, -1)",
            BOB,
            owner(None, None),
            Ok(SetAttr::default()),
        ),
        (
            "owner to own ids",
            ALICE,
            owner(Some(1000), Some(100)),
            Ok(SetAttr {
                uid: Some(1000),
                gid: Some(100),
                ctime: Some(now),
                ..SetAttr::default()
            }),
        ),
        (
            "stranger backdates",
            BOB,
            stamps(Some(Stamp::At(1)), None),
            denied,
        ),
        (
            "group member without write touches",
            Id::new(2000, 100),
            stamps(Some(Stamp::Now), Some(Stamp::Now)),
            Err(FsError::Access), // 0o644: the group has no write bit
        ),
        (
            "owner sets one stamp",
            ALICE,
            stamps(None, Some(Stamp::At(-3))),
            Ok(SetAttr {
                mtime: Some(-3),
                ctime: Some(now),
                ..SetAttr::default()
            }),
        ),
        (
            "both omitted",
            BOB,
            stamps(None, None),
            Ok(SetAttr::default()),
        ),
    ];
    for (name, id, request, want) in cases {
        let got = vfs::authorize(&file, *id, *request, now);
        check!(got == *want, "{name}: got {got:?}, want {want:?}");
    }

    // chown strips setuid/setgid from a file, not from a directory.
    let privileged = alice_meta(0o6755);
    let change = vfs::authorize(&privileged, Id::ROOT, owner(Some(0), None), now);
    check!(
        change.map(|change| change.mode) == Ok(Some(0o755)),
        "root's chown kept setuid: {change:?}"
    );
    let dir = Meta {
        mode: vfs::S_IFDIR | 0o2775,
        kind: FileKind::Dir,
        ..privileged
    };
    let change = vfs::authorize(&dir, Id::ROOT, owner(Some(0), None), now);
    check!(
        change.map(|change| change.mode) == Ok(None),
        "a directory lost its setgid: {change:?}"
    );

    // Write permission is enough to touch, not to set a time.
    let shared = alice_meta(0o664);
    let member = Id::new(2000, 100);
    let touch = stamps(Some(Stamp::Now), Some(Stamp::Now));
    check!(
        vfs::authorize(&shared, member, touch, now).is_ok(),
        "a group writer could not touch"
    );
    check!(
        vfs::authorize(&shared, member, stamps(Some(Stamp::Now), None), now) == denied,
        "a group writer touched one stamp"
    );
    Ok(())
}

/// Through the VFS: a change is visible to the next `stat` from the cache (no
/// stale mode after the refresh), a stranger is refused, and the parent's
/// search bit still gates the path form but not the open-descriptor form.
pub fn setattr_through_vfs_and_cache() -> Result<(), String> {
    let root = Id::ROOT;
    let mut vfs = ram_vfs();
    vfs.mkdir(root, "/home", 0o755).map_err(fs_error)?;
    vfs.create(root, "/home/f", 0o644).map_err(fs_error)?;
    vfs.setattr(root, "/home/f", owner(Some(1000), Some(100)))
        .map_err(fs_error)?;

    vfs.stat(root, "/home/f").map_err(fs_error)?; // warm the caches
    vfs.setattr(ALICE, "/home/f", AttrRequest::Mode(0o600))
        .map_err(fs_error)?;
    let misses = vfs.cache_stats().inode_misses;
    let meta = vfs.stat(root, "/home/f").map_err(fs_error)?;
    check!(meta.mode & 0o7777 == 0o600, "stat after chmod saw {meta:?}");
    check!(
        vfs.cache_stats().inode_misses == misses,
        "the refreshed entry was not served from the cache"
    );
    check!(
        vfs.setattr(BOB, "/home/f", AttrRequest::Mode(0o777)).err() == Some(FsError::NotPermitted),
        "a stranger's chmod"
    );

    vfs.setattr(root, "/home", AttrRequest::Mode(0o700))
        .map_err(fs_error)?;
    check!(
        vfs.setattr(ALICE, "/home/f", AttrRequest::Mode(0o640))
            .err()
            == Some(FsError::Access),
        "chmod through a directory the caller cannot search"
    );
    vfs.setattr_open(ALICE, "/home/f", AttrRequest::Mode(0o640))
        .map_err(fs_error)?;
    check!(
        vfs.stat(root, "/home/f").map_err(fs_error)?.mode & 0o7777 == 0o640,
        "the open-descriptor form did not land"
    );
    Ok(())
}

/// ramfs keeps the three timestamps: creation stamps all of them, a write or
/// truncate moves mtime and ctime but not atime, `setattr` sets any of them.
pub fn ramfs_timestamps() -> Result<(), String> {
    let fs = RamFs::new();
    let start = vfs::now();
    let meta = fs.create("f", 0o644, Id::ROOT).map_err(fs_error)?;
    let clock = start..=vfs::now();
    check!(
        clock.contains(&meta.times.atime) && meta.times.atime == meta.times.mtime,
        "fresh times are {:?}",
        meta.times
    );
    let past = SetAttr::times(vfs::Times {
        atime: -10,
        mtime: -20,
        ctime: -30,
    });
    let meta = fs.setattr("f", &past).map_err(fs_error)?;
    check!(
        (meta.times.atime, meta.times.mtime, meta.times.ctime) == (-10, -20, -30),
        "setattr gave {:?}",
        meta.times
    );
    fs.write("f", 0, b"x").map_err(fs_error)?;
    let meta = fs.lookup("f").map_err(fs_error)?;
    check!(
        meta.times.atime == -10 && meta.times.mtime >= 0 && meta.times.ctime >= 0,
        "a write gave {:?}",
        meta.times
    );
    fs.setattr("f", &past).map_err(fs_error)?;
    fs.truncate("f", 0).map_err(fs_error)?;
    let meta = fs.lookup("f").map_err(fs_error)?;
    check!(
        meta.times.atime == -10 && meta.times.mtime >= 0,
        "a truncate gave {:?}",
        meta.times
    );
    Ok(())
}
