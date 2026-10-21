//! Names through the Linux ABI mount table: hard links (issue #871) and
//! `unlink`, kept beside the other `abi_*` helpers in `mod.rs` to hold that
//! file under the size limit.

use super::coherence::{abi_changed, Change};
use super::vfs::{FsError, Id};
use super::{abi_with, hidden, openfile};

/// `link(existing, new)` as `id` ([`Vfs::link`](super::vfs::Vfs::link) has
/// the rules). The reserved namespace stays the kernel's: no new name may be
/// made in it (`EINVAL`, as for `create`), and a file parked there is one
/// that was unlinked while open, which a new name must not bring back
/// (`ENOENT`, what Linux answers for a file with no name left).
pub fn abi_link(id: Id, existing: &str, new: &str) -> Result<(), FsError> {
    hidden::refuse_reserved(new)?;
    if hidden::refuse_reserved(existing).is_err() {
        return Err(FsError::NotFound);
    }
    // A name is added while no open file's names are being decided on.
    let _names = openfile::names_lock();
    let linked = abi_with(|vfs| vfs.link(id, existing, new)).unwrap_or(Err(FsError::NotFound));
    // The new name now names the file; the old one's inode changed (its link
    // count and change time), which the native table must not answer stale.
    abi_changed(new, Change::Name, &linked);
    abi_changed(existing, Change::Content, &linked);
    linked
}

/// Remove a regular file through the Linux ABI VFS. A file that a descriptor
/// still has open loses its name but keeps its data until the last close
/// ([`openfile`]).
pub fn abi_unlink(id: Id, path: &str) -> Result<(), FsError> {
    let _names = openfile::names_lock();
    if openfile::unlink_open(id, path)? {
        return Ok(());
    }
    abi_unlink_raw(id, path)
}

/// Delete a name outright, with no regard for open descriptors (the hidden
/// entry of an unlinked file is deleted this way when its last one closes).
pub(super) fn abi_unlink_raw(id: Id, path: &str) -> Result<(), FsError> {
    let unlinked = abi_with(|vfs| vfs.unlink(id, path)).unwrap_or(Err(FsError::NotFound));
    abi_changed(path, Change::Name, &unlinked);
    unlinked
}
