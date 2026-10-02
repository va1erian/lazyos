//! The execute-permission gate a program passes before its image is read
//! (issue #507, section 5).
//!
//! A program runs only when its mount allows it (`noexec` binds root too), the
//! node is a regular file, and the caller holds an `x` bit for it. Root needs
//! at least one `x` bit as well ([`vfs::check_access`]), so a `0644` file a
//! user wrote cannot be started by init or any root service. The errors are
//! [`FsError`]s so each spawn path maps them to its own errno convention.

use crate::fs::{
    self,
    vfs::{self, FileKind, FsError, Id, Meta},
};

/// Whether the current task may run `path` from the native mount table, the
/// one native `spawn` reads its image from.
pub(crate) fn native(path: &str) -> Result<(), FsError> {
    if fs::mount_flags(path).noexec {
        return Err(FsError::Access);
    }
    regular(fs::vfs_check(Id::current(), path, vfs::EXECUTE)?)
}

/// Whether the current task may run `path` from the Linux ABI mount table.
pub(crate) fn abi(path: &str) -> Result<(), FsError> {
    if fs::abi_mount_flags(path).noexec {
        return Err(FsError::Access);
    }
    regular(fs::abi_check(Id::current(), path, vfs::EXECUTE)?)
}

/// The gate for a `linux:` spawn line, whose image the Linux loader looks up
/// in the ABI table first and the native table second.
///
/// A name with no node in either table is allowed through: it is a synthetic
/// applet name (`sh`, `/bin/ls`), which the loader maps to a build-placed
/// executable (BusyBox, or a program at the image root) or fails with
/// `ENOENT`. Every name that *is* a node must pass the check.
pub(crate) fn linux_spawn(path: &str) -> Result<(), FsError> {
    match abi(path) {
        Err(FsError::NotFound) => {}
        other => return other,
    }
    match native(path) {
        Err(FsError::NotFound) => Ok(()),
        other => other,
    }
}

/// Running a directory (or anything but a regular file) is `EACCES`, as for
/// Linux `execve`; search permission on it is no licence to execute it.
fn regular(meta: Meta) -> Result<(), FsError> {
    if meta.kind == FileKind::File {
        Ok(())
    } else {
        Err(FsError::Access)
    }
}
