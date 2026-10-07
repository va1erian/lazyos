//! `write_file` (17) on a file the caller owns in a directory it cannot
//! write (docs/accounts-plan.md U1): `accountsd`, as `_accounts`, rewrites the
//! passwd and group views it owns in root's `/system/etc`. Replacing needs the
//! file's write permission only, creating needs the directory's, as with
//! `open(O_CREAT | O_TRUNC)`.

use super::*;
use crate::fs::vfs::{AttrRequest, Id};

/// `_accounts`, the owner of the view.
const OWNER: u32 = 908;
const EACCES: i64 = 13;

/// A root 0755 directory holding `view`, owned by [`OWNER`] and 0644.
fn setup() -> Result<(), String> {
    fresh()?;
    let _ = crate::fs::vfs_unlink(Id::ROOT, "/tmp/ow/view");
    let _ = crate::fs::vfs_rmdir(Id::ROOT, "/tmp/ow");
    crate::fs::vfs_mkdir(Id::ROOT, "/tmp/ow", 0o755).map_err(|e| format!("mkdir: {e:?}"))?;
    crate::fs::vfs_setattr(Id::ROOT, "/tmp/ow", AttrRequest::Mode(0o755))
        .map_err(|e| format!("chmod dir: {e:?}"))?;
    crate::fs::vfs_create(Id::ROOT, "/tmp/ow/view", 0o644).map_err(|e| format!("create: {e:?}"))?;
    let owner = AttrRequest::Owner {
        uid: Some(OWNER),
        gid: Some(OWNER),
    };
    crate::fs::vfs_setattr(Id::ROOT, "/tmp/ow/view", owner).map_err(|e| format!("chown: {e:?}"))?;
    Ok(())
}

/// Run `body` as `uid` (no capability), then as the kernel again.
fn as_uid<R>(uid: u32, body: impl FnOnce() -> R) -> R {
    let me = task::current();
    credentials::set(me, Cred::new(uid, uid, 0, 0, 0));
    let result = body();
    credentials::reset_for_task(me);
    result
}

/// The owner replaces the file; nobody but root creates beside it; another
/// uid may not replace it; a refused replace leaves the bytes alone.
pub(super) fn owner_replaces_in_a_foreign_directory() -> Result<(), String> {
    setup()?;
    let _clean = Cleanup(&["/tmp/ow/view", "/tmp/ow/new", "/tmp/ow"]);
    let replaced = as_uid(OWNER, || put("/tmp/ow/view", b"user:1000"));
    check!(
        replaced == 9,
        "the owner could not replace its view: {replaced:#x}"
    );
    check!(
        slurp("/tmp/ow/view").as_deref() == Some(&b"user:1000"[..]),
        "the replaced bytes did not land"
    );
    let shorter = as_uid(OWNER, || put("/tmp/ow/view", b"a"));
    check!(shorter == 1, "a shorter replace failed: {shorter:#x}");
    check!(
        slurp("/tmp/ow/view").as_deref() == Some(&b"a"[..]),
        "a shorter replace kept the old tail"
    );
    let created = as_uid(OWNER, || put("/tmp/ow/new", b"x"));
    check!(
        created == failed(EACCES),
        "a non-root uid created a file in root's directory: {created:#x}"
    );
    let stranger = as_uid(1000, || put("/tmp/ow/view", b"evil"));
    check!(
        stranger == failed(EACCES),
        "another uid replaced the owner's file: {stranger:#x}"
    );
    check!(
        slurp("/tmp/ow/view").as_deref() == Some(&b"a"[..]),
        "a refused replace damaged the file"
    );
    Ok(())
}

/// What the Linux ABI table reports for `path`: its size and whole contents.
fn abi_view(path: &str) -> Result<(u64, Vec<u8>), String> {
    let size = crate::fs::abi_stat(Id::ROOT, path).map_err(|e| format!("abi stat: {e:?}"))?;
    let bytes = crate::fs::abi_read(Id::ROOT, path).map_err(|e| format!("abi read: {e:?}"))?;
    Ok((size.size, bytes))
}

/// A native rewrite is seen by the Linux ABI table at once (each table keeps
/// its own metadata cache): a Linux `stat` and whole-file read of the view
/// `accountsd` rewrote must have the new size, not the cached old one.
pub(super) fn abi_sees_native_rewrites() -> Result<(), String> {
    setup()?;
    let _clean = Cleanup(&["/tmp/ow/view", "/tmp/ow"]);
    check!(
        as_uid(OWNER, || put("/tmp/ow/view", b"user:1000")) == 9,
        "first write"
    );
    check!(
        abi_view("/tmp/ow/view")? == (9, b"user:1000".to_vec()),
        "first view"
    );
    let longer = b"user:1000\nbob:1001\n";
    check!(
        as_uid(OWNER, || put("/tmp/ow/view", longer)) == longer.len() as u64,
        "rewrite"
    );
    let (size, bytes) = abi_view("/tmp/ow/view")?;
    check!(
        size == longer.len() as u64 && bytes == longer,
        "the ABI table kept the old size: {size} bytes, {bytes:?}"
    );
    Ok(())
}

/// Soak: the owner rewrites its view thousands of times; no frame leaks, the
/// last bytes win, and the Linux ABI table follows every size change.
pub(super) fn soak_owner_rewrites() -> Result<(), String> {
    setup()?;
    let _clean = Cleanup(&["/tmp/ow/view", "/tmp/ow"]);
    let before = mem::frame_stats().live();
    for round in 0..2000u32 {
        let body = vec![b'a' + (round % 26) as u8; (round % 700) as usize];
        let wrote = as_uid(OWNER, || put("/tmp/ow/view", &body));
        check!(
            wrote == body.len() as u64,
            "round {round}: wrote {wrote:#x}"
        );
        if round % 97 == 0 {
            let (size, _) = abi_view("/tmp/ow/view")?;
            check!(size == body.len() as u64, "round {round}: ABI size {size}");
        }
    }
    let last = vec![b'a' + (1999 % 26) as u8; 1999 % 700];
    check!(
        slurp("/tmp/ow/view").as_deref() == Some(&last[..]),
        "the last rewrite is not what the file holds"
    );
    let after = mem::frame_stats().live();
    check!(after <= before + 8, "frames leaked: {before} -> {after}");
    Ok(())
}
