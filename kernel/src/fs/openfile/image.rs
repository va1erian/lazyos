//! A running program's file: the page cache of its image is shared by every
//! process that runs it (`process::loader`), so the file must not change
//! under them.
//!
//! The loader holds the file open through an [`OpenFile`] of role
//! [`Role::Image`] and maps the program's read-only segments from the file's
//! page cache. That is the same promise Linux makes with `ETXTBSY`:
//!
//! * while any image description lives, nothing may open the file for
//!   writing, write or truncate it by name, or write through an old
//!   descriptor (there is none: opening for writing is refused, and a file
//!   that is already open for writing is not shared, the loader copies it);
//! * the *name* may still go: `unlink` and `rename` over the file leave the
//!   running inode under a hidden name until the last process (and mapping)
//!   is gone, which is how a program is replaced while it runs. The same
//!   holds for a native `unlink` or `rename` ([`unlink_text`],
//!   [`displace_text`]), which do not consult the registry otherwise.
//!
//! The counts live in the registry's [`Inode`] and change under the
//! [`OPEN`](super::OPEN) lock, the lock that also decides whether a file is
//! shared, so a claim and a conflicting claim cannot both succeed.

use alloc::string::String;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::{canonical, find, orphan, share_inode, Displaced, Inode, OpenFile};
use crate::fs::vfs::{FsError, Id, Node};

/// What an open description does to its file's counts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Role {
    /// Reads only: no claim.
    Reader,
    /// Open for writing: refused while the file is a running program.
    Writer,
    /// Runs the file as a program's image: refused while it is open for
    /// writing.
    Image,
}

/// Image descriptions alive anywhere: the by-name checks skip the registry
/// when it is zero, which is almost always.
static TEXT_FILES: AtomicUsize = AtomicUsize::new(0);

/// Take `role`'s claim on `inode`, or refuse it for the other kind's.
/// Called with the registry lock held.
pub(super) fn claim(inode: &Inode, role: Role) -> Result<(), FsError> {
    match role {
        Role::Reader => {}
        Role::Writer => {
            if inode.text.load(Ordering::Relaxed) > 0 {
                return Err(FsError::TextBusy);
            }
            inode.writers.fetch_add(1, Ordering::Relaxed);
        }
        Role::Image => {
            if inode.writers.load(Ordering::Relaxed) > 0 {
                return Err(FsError::TextBusy);
            }
            inode.text.fetch_add(1, Ordering::Relaxed);
            TEXT_FILES.fetch_add(1, Ordering::Relaxed);
        }
    }
    Ok(())
}

/// Give back what [`claim`] took. Called with the registry lock held.
pub(super) fn unclaim(inode: &Inode, role: Role) {
    match role {
        Role::Reader => {}
        Role::Writer => {
            inode.writers.fetch_sub(1, Ordering::Relaxed);
        }
        Role::Image => {
            inode.text.fetch_sub(1, Ordering::Relaxed);
            TEXT_FILES.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl OpenFile {
    /// Open `node`, the file at `path`, as a program's image of `len` bytes.
    /// Refused with [`FsError::TextBusy`] while some description has the
    /// file open for writing, and with [`FsError::Invalid`] when the file is
    /// no longer the `len` bytes the loader read the headers of: the caller
    /// then loads by copying.
    pub fn open_image(path: &str, node: Node, len: u64) -> Result<Arc<OpenFile>, FsError> {
        let shown = String::from(path);
        let inode = share_inode(canonical(path), shown, Some(node), Role::Image)?;
        let file = Arc::new(OpenFile {
            inode,
            offset: AtomicU64::new(0),
            readable: true,
            writable: false,
            append: false,
            role: Role::Image,
        });
        // Claimed first, so the size cannot change again behind this check.
        match file.stat() {
            Ok(meta) if meta.size == len => Ok(file),
            _ => Err(FsError::Invalid),
        }
    }
}

/// The running program at `path`, if there is one.
fn running(path: &str) -> Option<Arc<Inode>> {
    if TEXT_FILES.load(Ordering::Relaxed) == 0 {
        return None;
    }
    find(path).filter(|inode| inode.text.load(Ordering::Relaxed) > 0)
}

/// A write or truncate of `path` by name: refused while it is a running
/// program (`ETXTBSY`).
pub(in crate::fs) fn refuse_text_write(path: &str) -> Result<(), FsError> {
    match running(path) {
        Some(_) => Err(FsError::TextBusy),
        None => Ok(()),
    }
}

/// A native `unlink` of `path`: if it is a running program, take its name
/// away without freeing it and report `true` (the caller deletes nothing).
pub(in crate::fs) fn unlink_text(id: Id, path: &str) -> Result<bool, FsError> {
    match running(path) {
        // Already parked: its name is gone, and what remains is not for
        // anyone to delete while the program runs.
        Some(inode) if inode.orphan.load(Ordering::Relaxed) => Ok(true),
        Some(inode) => orphan(id, &inode).map(|_| true),
        None => Ok(false),
    }
}

/// The native `rmdir` of `path` ended with `removed`. A directory a running
/// program was unlinked from holds its parked hidden entry, which the native
/// table lists: the Linux ABI's rule applies (the parked file does not make
/// the directory non-empty, [`super::rmdir`]).
pub(in crate::fs) fn rmdir_parked(
    id: Id,
    path: &str,
    removed: Result<(), FsError>,
) -> Result<(), FsError> {
    match removed {
        Err(FsError::NotEmpty) if TEXT_FILES.load(Ordering::Relaxed) > 0 => super::rmdir(id, path),
        other => other,
    }
}

/// Before a native `rename(_, to)` replaces `to`: if `to` is a running
/// program, move it out of the way as [`super::displace`] does.
pub(in crate::fs) fn displace_text(id: Id, to: &str) -> Result<Option<Displaced>, FsError> {
    match running(to) {
        Some(_) => super::displace(id, to),
        None => Ok(None),
    }
}
