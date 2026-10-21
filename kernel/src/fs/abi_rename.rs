//! Renames through the Linux ABI mount table (kept beside, not inside, the
//! other `abi_*` helpers in `mod.rs` to hold that file under the size limit).

use super::coherence::{abi_changed, Change};
use super::vfs::{FsError, Id};
use super::{abi_with, hidden, openfile};

/// Rename within one mount through the Linux ABI VFS. Open files follow their
/// name, and one that the rename replaces is unlinked, not destroyed
/// ([`openfile`]).
pub fn abi_rename(id: Id, from: &str, to: &str) -> Result<(), FsError> {
    hidden::refuse_reserved(to)?;
    let _names = openfile::names_lock();
    // Two spellings of one name (`/tmp/a` and `/transient/a`), or two hard
    // links of one file, are no replacement: `displace` tells, by the file.
    let displaced = openfile::displace(id, from, to)?;
    let renamed = abi_rename_raw(id, from, to);
    if let Some(displaced) = displaced {
        displaced.settle(renamed.is_ok());
    }
    if renamed.is_ok() {
        openfile::retarget(from, to);
    }
    renamed
}

/// Rename a name with no regard for open descriptors.
pub(crate) fn abi_rename_raw(id: Id, from: &str, to: &str) -> Result<(), FsError> {
    let renamed = abi_with(|vfs| vfs.rename(id, from, to)).unwrap_or(Err(FsError::NotFound));
    abi_changed(from, Change::Name, &renamed);
    abi_changed(to, Change::Name, &renamed);
    renamed
}
