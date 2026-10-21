//! Hard links through the Linux ABI mount table (issue #871), kept beside
//! the other `abi_*` helpers in `mod.rs` to hold that file under the size
//! limit.

use super::coherence::{abi_changed, Change};
use super::vfs::{FsError, Id};
use super::{abi_with, hidden};

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
    let linked = abi_with(|vfs| vfs.link(id, existing, new)).unwrap_or(Err(FsError::NotFound));
    // The new name now names the file; the old one's inode changed (its link
    // count and change time), which the native table must not answer stale.
    abi_changed(new, Change::Name, &linked);
    abi_changed(existing, Change::Content, &linked);
    linked
}
