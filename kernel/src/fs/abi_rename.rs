//! Renames through the Linux ABI mount table (kept beside, not inside, the
//! other `abi_*` helpers in `mod.rs` to hold that file under the size limit).

use super::coherence::{abi_changed, Change};
use super::vfs::{FsError, Id};
use super::{abi_with, hidden, nodes, openfile};

/// Rename within one mount through the Linux ABI VFS. Open files follow their
/// name, and one that the rename replaces is unlinked, not destroyed
/// ([`openfile`]).
pub fn abi_rename(id: Id, from: &str, to: &str) -> Result<(), FsError> {
    hidden::refuse_reserved(to)?;
    // Two names of one file are the same file here (`/tmp/a` and
    // `/transient/a`): comparing the text would orphan the source as if it
    // were the file being replaced.
    let same = nodes::abi_canonical(from) == nodes::abi_canonical(to);
    let displaced = if same {
        None
    } else {
        openfile::displace(id, to)?
    };
    match abi_rename_raw(id, from, to) {
        Ok(()) => {
            openfile::retarget(from, to);
            Ok(())
        }
        Err(error) => {
            if let Some(displaced) = displaced {
                displaced.restore();
            }
            Err(error)
        }
    }
}

/// Rename a name with no regard for open descriptors.
pub(crate) fn abi_rename_raw(id: Id, from: &str, to: &str) -> Result<(), FsError> {
    let renamed = abi_with(|vfs| vfs.rename(id, from, to)).unwrap_or(Err(FsError::NotFound));
    abi_changed(from, Change::Name, &renamed);
    abi_changed(to, Change::Name, &renamed);
    renamed
}
