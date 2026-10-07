//! The native and Linux ABI mount tables keep separate metadata caches over
//! the same volumes (`fs/coherence.rs`): a mutation through either table must
//! be seen by the other at once. `/tmp` is one ramfs mounted in both, which is
//! exactly the configured layout's sharing.
//!
//! The dangerous direction is a tightening: a native `chmod` that takes a
//! file away from a user, still answered from the ABI table's cache, would
//! let that user's Linux `access`/`open` through.

use super::*;
use crate::fs::vfs::{AttrRequest, FileKind, FsError, Id, READ};

const DIR: &str = "/tmp/coh";
const FILE: &str = "/tmp/coh/f";
const SUB: &str = "/tmp/coh/d";
const MOVED: &str = "/tmp/coh/g";
const USER: Id = Id::new(1000, 1000);

/// A root 0755 directory holding a root 0644 file with four bytes.
fn setup() -> Result<(), String> {
    fresh()?;
    teardown();
    crate::fs::vfs_mkdir(Id::ROOT, DIR, 0o755).map_err(|e| format!("mkdir: {e:?}"))?;
    chmod_native(DIR, 0o755)?;
    crate::fs::vfs_create(Id::ROOT, FILE, 0o644).map_err(|e| format!("create: {e:?}"))?;
    chmod_native(FILE, 0o644)?;
    crate::fs::vfs_write(Id::ROOT, FILE, 0, b"data").map_err(|e| format!("write: {e:?}"))?;
    Ok(())
}

/// Remove whatever a run left, through both tables.
fn teardown() {
    for file in [FILE, MOVED] {
        let _ = crate::fs::vfs_unlink(Id::ROOT, file);
    }
    let _ = crate::fs::vfs_rmdir(Id::ROOT, SUB);
    let _ = crate::fs::vfs_rmdir(Id::ROOT, DIR);
}

struct Teardown;

impl Drop for Teardown {
    fn drop(&mut self) {
        teardown();
    }
}

fn chmod_native(path: &str, mode: u16) -> Result<(), String> {
    crate::fs::vfs_setattr(Id::ROOT, path, AttrRequest::Mode(mode))
        .map(|_| ())
        .map_err(|e| format!("native chmod {path} {mode:o}: {e:?}"))
}

fn chmod_abi(path: &str, mode: u16) -> Result<(), String> {
    crate::fs::abi_setattr(Id::ROOT, path, AttrRequest::Mode(mode))
        .map(|_| ())
        .map_err(|e| format!("abi chmod {path} {mode:o}: {e:?}"))
}

/// The user's read check through the ABI table (`access(R_OK)`), warming its
/// cache when it passes.
fn abi_readable() -> Result<(), FsError> {
    crate::fs::abi_check(USER, FILE, READ).map(|_| ())
}

fn native_readable() -> Result<(), FsError> {
    crate::fs::vfs_check(USER, FILE, READ).map(|_| ())
}

/// A native `chmod`/`chown` is seen by a Linux `access` and `open` at once,
/// including a `chmod` on the directory that holds the file.
pub(super) fn abi_sees_native_attributes() -> Result<(), String> {
    setup()?;
    let _clean = Teardown;
    check!(
        abi_readable().is_ok(),
        "the user could not read a 0644 file"
    );
    chmod_native(FILE, 0o600)?;
    check!(
        abi_readable() == Err(FsError::Access),
        "a native chmod 0600 still let a Linux access through: {:?}",
        abi_readable()
    );
    check!(
        crate::fs::abi_read(USER, FILE) == Err(FsError::Access),
        "a native chmod 0600 still let a Linux open read the file"
    );
    let owner = AttrRequest::Owner {
        uid: Some(USER.uid),
        gid: Some(USER.gid),
    };
    crate::fs::vfs_setattr(Id::ROOT, FILE, owner).map_err(|e| format!("chown: {e:?}"))?;
    let meta = crate::fs::abi_stat(Id::ROOT, FILE).map_err(|e| format!("abi stat: {e:?}"))?;
    check!(
        meta.uid == USER.uid,
        "the ABI kept the old owner {}",
        meta.uid
    );
    check!(abi_readable().is_ok(), "the new owner was refused");
    chmod_native(DIR, 0o700)?;
    check!(
        abi_readable() == Err(FsError::Access),
        "a native chmod 0700 on the directory was not seen: {:?}",
        abi_readable()
    );
    Ok(())
}

/// A native `mkdir`/`rmdir` is seen by the ABI table at once: a removed
/// directory no longer stats, and a file made in its place is a file.
pub(super) fn abi_sees_native_directories() -> Result<(), String> {
    setup()?;
    let _clean = Teardown;
    crate::fs::vfs_mkdir(Id::ROOT, SUB, 0o755).map_err(|e| format!("mkdir: {e:?}"))?;
    let made = crate::fs::abi_stat(Id::ROOT, SUB).map_err(|e| format!("abi stat: {e:?}"))?;
    check!(made.kind == FileKind::Dir, "the ABI saw {:?}", made.kind);
    crate::fs::vfs_rmdir(Id::ROOT, SUB).map_err(|e| format!("rmdir: {e:?}"))?;
    check!(
        crate::fs::abi_stat(Id::ROOT, SUB) == Err(FsError::NotFound),
        "the ABI still stats a directory a native rmdir removed"
    );
    crate::fs::vfs_mkdir(Id::ROOT, SUB, 0o700).map_err(|e| format!("mkdir again: {e:?}"))?;
    let again = crate::fs::abi_stat(Id::ROOT, SUB).map_err(|e| format!("abi stat: {e:?}"))?;
    check!(
        again.mode & 0o777 == 0o700,
        "the ABI kept the first directory's mode {:o}",
        again.mode
    );
    Ok(())
}

/// The other direction: every Linux mutation is seen by native tasks.
pub(super) fn native_sees_abi_mutations() -> Result<(), String> {
    setup()?;
    let _clean = Teardown;
    check!(
        native_readable().is_ok(),
        "the user could not read natively"
    );
    chmod_abi(FILE, 0o600)?;
    check!(
        native_readable() == Err(FsError::Access),
        "a Linux chmod 0600 was not seen natively: {:?}",
        native_readable()
    );
    chmod_abi(FILE, 0o644)?;
    check!(native_readable().is_ok(), "a Linux chmod back was not seen");
    let size = |path| crate::fs::vfs_stat(Id::ROOT, path).map(|meta| meta.size);
    check!(size(FILE) == Ok(4), "first size {:?}", size(FILE));
    crate::fs::abi_write(Id::ROOT, FILE, 4, b" and more").map_err(|e| format!("w: {e:?}"))?;
    check!(
        size(FILE) == Ok(13),
        "native size after a Linux write {:?}",
        size(FILE)
    );
    crate::fs::abi_truncate(Id::ROOT, FILE, 2).map_err(|e| format!("trunc: {e:?}"))?;
    check!(
        size(FILE) == Ok(2),
        "native size after a Linux truncate {:?}",
        size(FILE)
    );
    crate::fs::abi_mkdir(Id::ROOT, SUB, 0o755).map_err(|e| format!("abi mkdir: {e:?}"))?;
    check!(size(SUB).is_ok(), "a Linux mkdir was not seen natively");
    crate::fs::abi_rmdir(Id::ROOT, SUB).map_err(|e| format!("abi rmdir: {e:?}"))?;
    check!(
        size(SUB) == Err(FsError::NotFound),
        "a Linux rmdir was not seen natively"
    );
    crate::fs::abi_rename(Id::ROOT, FILE, MOVED).map_err(|e| format!("rename: {e:?}"))?;
    check!(
        size(FILE) == Err(FsError::NotFound) && size(MOVED) == Ok(2),
        "a Linux rename was not seen natively: {:?} {:?}",
        size(FILE),
        size(MOVED)
    );
    crate::fs::abi_unlink(Id::ROOT, MOVED).map_err(|e| format!("unlink: {e:?}"))?;
    check!(
        size(MOVED) == Err(FsError::NotFound),
        "a Linux unlink was not seen natively"
    );
    Ok(())
}

/// Soak: thousands of alternating tightenings and loosenings, from either
/// table, each checked on the other at once, with directories made and
/// removed between; nothing stale is ever answered and no frame leaks.
pub(super) fn soak_cross_table_coherence() -> Result<(), String> {
    setup()?;
    let _clean = Teardown;
    let before = mem::frame_stats().live();
    for round in 0..2000u32 {
        let tight = round % 2 == 0;
        let mode = if tight { 0o600 } else { 0o644 };
        let native_side = round % 3 == 0;
        if native_side {
            chmod_native(FILE, mode)?;
        } else {
            chmod_abi(FILE, mode)?;
        }
        let (seen, other) = if native_side {
            (abi_readable(), "ABI")
        } else {
            (native_readable(), "native")
        };
        check!(
            seen.is_err() == tight,
            "round {round}: the {other} table answered {seen:?} for mode {mode:o}"
        );
        if round % 5 == 0 {
            if native_side {
                crate::fs::vfs_mkdir(Id::ROOT, SUB, 0o755).map_err(|e| format!("{e:?}"))?;
                crate::fs::abi_stat(Id::ROOT, SUB).map_err(|e| format!("r{round}: {e:?}"))?;
                crate::fs::vfs_rmdir(Id::ROOT, SUB).map_err(|e| format!("{e:?}"))?;
                check!(
                    crate::fs::abi_stat(Id::ROOT, SUB) == Err(FsError::NotFound),
                    "round {round}: the ABI kept a removed directory"
                );
            } else {
                crate::fs::abi_mkdir(Id::ROOT, SUB, 0o755).map_err(|e| format!("{e:?}"))?;
                crate::fs::vfs_stat(Id::ROOT, SUB).map_err(|e| format!("r{round}: {e:?}"))?;
                crate::fs::abi_rmdir(Id::ROOT, SUB).map_err(|e| format!("{e:?}"))?;
                check!(
                    crate::fs::vfs_stat(Id::ROOT, SUB) == Err(FsError::NotFound),
                    "round {round}: the native table kept a removed directory"
                );
            }
        }
    }
    let after = mem::frame_stats().live();
    check!(after <= before + 8, "frames leaked: {before} -> {after}");
    Ok(())
}
