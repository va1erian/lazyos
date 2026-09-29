//! `chmod`, `chown` and `utimensat` on the [`Vfs`]: the POSIX rules for who may
//! change which attribute, and the one [`Filesystem::setattr`] call that
//! applies the result.
//!
//! Split from `vfs.rs` (which keeps resolution and the caches) to hold that
//! file under the size limit, and because the rules are a responsibility of
//! their own: every backend gets the same, already-authorized [`SetAttr`].
//!
//! [`Filesystem::setattr`]: super::Filesystem::setattr

use alloc::sync::Arc;

use super::attr::{now, AttrRequest, SetAttr, Stamp};
use super::{check_access, FileKind, FsError, Id, Meta, Path, Vfs, S_ISGID, S_ISUID, WRITE};

impl Vfs {
    /// Change an attribute of `path` (`chmod`/`chown`/`utimensat`). Every
    /// ancestor needs search permission, as for any lookup; the node itself
    /// needs whatever the request's rule demands (see [`authorize`]). Returns
    /// the node's metadata after the change.
    pub fn setattr(&mut self, id: Id, path: &str, request: AttrRequest) -> Result<Meta, FsError> {
        let path = Path::parse(path);
        let meta = self.check_path(id, &path, 0)?;
        self.apply_attr(id, &path, meta, request)
    }

    /// [`Vfs::setattr`] on a node the caller holds open (`fchmod`, `fchown`,
    /// `futimens`). The name was searched when the descriptor was opened, and
    /// POSIX does not ask again: a process keeps the files it opened even when
    /// it can no longer reach their directory. Only the request's own rule is
    /// checked.
    pub fn setattr_open(
        &mut self,
        id: Id,
        path: &str,
        request: AttrRequest,
    ) -> Result<Meta, FsError> {
        let path = Path::parse(path);
        let meta = self.stat_path(&path)?;
        self.apply_attr(id, &path, meta, request)
    }

    /// Authorize `request` against `meta`, hand the change to the backend, and
    /// refresh the caches with what it reports.
    fn apply_attr(
        &mut self,
        id: Id,
        path: &Path,
        meta: Meta,
        request: AttrRequest,
    ) -> Result<Meta, FsError> {
        let change = authorize(&meta, id, request, now())?;
        if change.is_empty() {
            return Ok(meta); // e.g. `chown(-1, -1)`: nothing to write
        }
        let (mount, rel) = self.resolve_mount(path)?;
        let fs = Arc::clone(&self.mounts[mount].fs);
        let result = fs.setattr(&rel, &change);
        // Invalidate even on failure: the overlay may have copied the node
        // (and, for a directory, its subtree) up before the change failed, and
        // every cached inode number below `path` is stale then.
        self.invalidate_mount_path(mount, &rel);
        let updated = result?;
        self.insert_cache(mount, &rel, updated);
        Ok(updated)
    }
}

/// Turn a request into the change a backend applies, or refuse it.
///
/// `NotPermitted` (`EPERM`) means the caller lacks the ownership the rule
/// needs; `Access` (`EACCES`) is only for the `touch` case, whose fallback is
/// the ordinary write-permission check.
pub fn authorize(meta: &Meta, id: Id, request: AttrRequest, now: i64) -> Result<SetAttr, FsError> {
    match request {
        AttrRequest::Mode(mode) => chmod_change(meta, id, mode, now),
        AttrRequest::Owner { uid, gid } => chown_change(meta, id, uid, gid, now),
        AttrRequest::Times { atime, mtime } => utimes_change(meta, id, atime, mtime, now),
    }
}

/// Root, or the node's owner.
fn owns(meta: &Meta, id: Id) -> bool {
    id.is_root() || id.uid == meta.uid
}

/// `chmod`: only the owner or root. A caller outside the file's group cannot
/// set setgid on it (the bit would hand out that group's rights); like Linux,
/// the bit is dropped silently rather than failing the whole call.
fn chmod_change(meta: &Meta, id: Id, mode: u16, now: i64) -> Result<SetAttr, FsError> {
    if !owns(meta, id) {
        return Err(FsError::NotPermitted);
    }
    let mut mode = mode & 0o7777;
    if !id.is_root() && id.gid != meta.gid {
        mode &= !S_ISGID;
    }
    Ok(SetAttr {
        mode: Some(mode),
        ctime: Some(now),
        ..SetAttr::default()
    })
}

/// `chown`: only root gives a file away (a uid other than the current one);
/// the owner may "change" the uid to itself and move the file to their own
/// group (there are no supplementary groups yet, so that is the caller's
/// gid) or keep its group.
///
/// A regular file that changes hands loses setuid and setgid, root's chown
/// included: otherwise the new owner (or group) would inherit a privileged
/// program they never vetted. Linux keeps a setgid bit without group execute
/// (its mandatory-locking marker); LazyOS has no mandatory locking, so both
/// bits go.
fn chown_change(
    meta: &Meta,
    id: Id,
    uid: Option<u32>,
    gid: Option<u32>,
    now: i64,
) -> Result<SetAttr, FsError> {
    if uid.is_none() && gid.is_none() {
        return Ok(SetAttr::default());
    }
    let is_owner = id.uid == meta.uid;
    let uid_ok = uid.is_none_or(|uid| id.is_root() || (is_owner && uid == meta.uid));
    let gid_ok =
        gid.is_none_or(|gid| id.is_root() || (is_owner && (gid == id.gid || gid == meta.gid)));
    if !uid_ok || !gid_ok {
        return Err(FsError::NotPermitted);
    }
    let privileged = meta.mode & (S_ISUID | S_ISGID);
    let mode = (meta.kind == FileKind::File && privileged != 0)
        .then_some(meta.mode & 0o7777 & !(S_ISUID | S_ISGID));
    Ok(SetAttr {
        mode,
        uid,
        gid,
        ctime: Some(now),
        ..SetAttr::default()
    })
}

/// `utimensat`: setting both stamps to "now" (`touch`) needs ownership or
/// write permission, since anyone who may write the file could bump its
/// mtime anyway; any explicit time, or touching just one stamp, needs
/// ownership, since it can make a file look older or newer than it is.
fn utimes_change(
    meta: &Meta,
    id: Id,
    atime: Option<Stamp>,
    mtime: Option<Stamp>,
    now: i64,
) -> Result<SetAttr, FsError> {
    if atime.is_none() && mtime.is_none() {
        return Ok(SetAttr::default()); // both UTIME_OMIT
    }
    let touch = atime == Some(Stamp::Now) && mtime == Some(Stamp::Now);
    if !owns(meta, id) {
        if !touch {
            return Err(FsError::NotPermitted);
        }
        check_access(meta, id, WRITE)?;
    }
    let resolve = |stamp: Stamp| match stamp {
        Stamp::Now => now,
        Stamp::At(time) => time,
    };
    Ok(SetAttr {
        atime: atime.map(resolve),
        mtime: mtime.map(resolve),
        ctime: Some(now),
        ..SetAttr::default()
    })
}
