//! Attribute changes on the copy-up overlay (issue #345): the node is copied
//! up first (a directory with its subtree), the lower layer never changes,
//! and a copy keeps the lower node's times until the caller changes them.

use super::*;
use crate::fs::vfs::{AttrRequest, SetAttr, Stamp, Times};

/// The lower node's times, stamped explicitly so a copy-up that dropped them
/// would show.
const LOWER_TIMES: Times = Times {
    atime: 11,
    mtime: 22,
    ctime: 33,
};

pub fn setattr_copies_up() -> Result<(), String> {
    let lower = lower_fixture()?;
    for path in ["HELLO.TXT", "LDIR/INNER.TXT"] {
        lower
            .setattr(path, &SetAttr::times(LOWER_TIMES))
            .map_err(fs_error)?;
    }
    let (mut vfs, overlay) = mounted_overlay(lower.clone())?;
    let root = Id::ROOT;

    // chmod copies the file up; the lower copy keeps its mode.
    let meta = vfs
        .setattr(root, "/HELLO.TXT", AttrRequest::Mode(0o640))
        .map_err(fs_error)?;
    check!(meta.mode & 0o7777 == 0o640, "overlay mode is {meta:?}");
    check!(
        meta.times.mtime == 22,
        "copy-up lost the lower mtime: {meta:?}"
    );
    check!(
        lower.lookup("HELLO.TXT").map_err(fs_error)?.mode & 0o7777 == 0o555,
        "the lower file was chmodded"
    );
    check!(
        vfs.read_file(root, "/HELLO.TXT").map_err(fs_error)? == b"Hello from LazyOS",
        "the copied-up file lost its bytes"
    );

    // Times on a file in a lower directory copy the directory up with it.
    let times = AttrRequest::Times {
        atime: Some(Stamp::At(100)),
        mtime: None,
    };
    let meta = vfs
        .setattr(root, "/LDIR/INNER.TXT", times)
        .map_err(fs_error)?;
    check!(
        meta.times.atime == 100,
        "utimes on a lower file gave {meta:?}"
    );
    check!(
        lower.lookup("LDIR/INNER.TXT").map_err(fs_error)?.times == LOWER_TIMES,
        "the lower file's times changed"
    );

    // A lower directory: chmod copies it up and its children stay listed.
    vfs.setattr(root, "/LDIR", AttrRequest::Mode(0o700))
        .map_err(fs_error)?;
    check!(
        has(&vfs.readdir(root, "/LDIR").map_err(fs_error)?, "INNER.TXT"),
        "a chmodded directory lost its children"
    );
    check!(
        vfs.stat(root, "/LDIR").map_err(fs_error)?.mode & 0o7777 == 0o700,
        "the directory mode did not land"
    );
    check!(
        lower.lookup("LDIR").map_err(fs_error)?.mode & 0o7777 == 0o555,
        "the lower directory was chmodded"
    );
    let (_, nodes) = overlay.usage();
    check!(nodes > 0, "nothing was copied up");
    Ok(())
}
