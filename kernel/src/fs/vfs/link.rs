//! `link`: a further name for a regular file (issue #871).
//!
//! The rules are Linux's, `fs.protected_hardlinks` included:
//!
//! * search on every ancestor of both paths, write and search on the new
//!   name's directory; the new name must not exist (`EEXIST`);
//! * a directory cannot be linked (`EPERM`);
//! * both names on one mount (`EXDEV` otherwise, also for the two mount
//!   points of one filesystem, `/tmp` and `/transient`, as Linux compares
//!   mounts), a writable one (`EROFS`);
//! * a caller who does not own the file may link it only when it is not
//!   set-user-ID, not set-group-ID and group-executable, and the caller may
//!   read *and* write it (`EPERM` otherwise). Without that a user could pin a
//!   file another user is about to delete, or plant a name for a privileged
//!   program's file in a directory they control;
//! * a confined label ([`crate::ipc::confine`]) must be allowed to *write*
//!   the existing name, the most any name of a file can give it: a link never
//!   reaches through a new path what the rules refuse through the old one.
//!
//! Afterwards both names' cached metadata is dropped: the link count and the
//! change time of the inode moved, and every cached name of it shares that
//! inode's entry.

use super::{
    check_access, confine_gate, FileKind, FsError, Id, Meta, Path, Vfs, EXECUTE, READ, S_ISGID,
    S_ISUID, WRITE,
};

impl Vfs {
    /// Give the regular file at `existing` the name `new`, as `id`.
    pub fn link(&mut self, id: Id, existing: &str, new: &str) -> Result<(), FsError> {
        let old = Path::parse(existing);
        let new = Path::parse(new);
        if new.is_root() {
            return Err(FsError::Exists);
        }
        let source = self.check_path(id, &old, 0)?;
        if source.kind == FileKind::Dir {
            return Err(FsError::NotPermitted);
        }
        confine_gate(id, &old, crate::ipc::confine::Access::Write)?;
        self.check_path(id, &new.parent(), WRITE | EXECUTE)?;
        if self.stat_path(&new).is_ok() {
            return Err(FsError::Exists);
        }
        may_link(id, &source)?;
        let (from_mount, from_rel) = self.resolve_writable(&old)?;
        let (to_mount, to_rel) = self.resolve_writable(&new)?;
        if from_mount != to_mount {
            return Err(FsError::CrossDevice);
        }
        let fs = alloc::sync::Arc::clone(&self.mounts[from_mount].fs);
        let linked = fs.link(&from_rel, &to_rel, id);
        // Even a failure may have raised the count before it gave up (ext2
        // writes the count first), so the cached inode goes either way.
        self.invalidate_mount_path(from_mount, &from_rel);
        self.invalidate_mount_path(to_mount, &to_rel);
        linked
    }
}

/// `fs.protected_hardlinks`: whether `id` may make a new name for `file`.
fn may_link(id: Id, file: &Meta) -> Result<(), FsError> {
    if id.is_root() || id.uid == file.uid {
        return Ok(());
    }
    let setgid_exec = file.mode & S_ISGID != 0 && file.mode & 0o010 != 0;
    if file.mode & S_ISUID != 0 || setgid_exec {
        return Err(FsError::NotPermitted);
    }
    check_access(file, id, READ | WRITE).map_err(|_| FsError::NotPermitted)
}
