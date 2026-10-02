//! The reserved namespace of hidden `.unlinked-<n>` entries.
//!
//! `unlink` of a file that is still open parks it under this name instead of
//! freeing it ([`openfile`](super::openfile)), and the next mount of a volume
//! that stopped uncleanly deletes whatever was left parked
//! ([`Ext2::reclaim_orphans`](super::ext2::Ext2::reclaim_orphans)). The reclaim
//! may only delete what the kernel itself parked, so the prefix belongs to the
//! kernel: no caller can *create* a name in it (`open(O_CREAT)`, `mkdir`, or a
//! `rename` onto it), and every such attempt answers [`FsError::Invalid`].
//! Existing entries can still be read, renamed away and unlinked, so nothing
//! that ended up there (an image made elsewhere, say) is trapped.

use super::vfs::FsError;

/// Names beginning with this are the kernel's to hand out.
pub const PREFIX: &str = ext2fs::ORPHAN_PREFIX;

/// Whether a directory-entry name lies in the reserved namespace.
pub fn is_reserved(name: &str) -> bool {
    name.starts_with(PREFIX)
}

/// Refuse to create `path` when its final component is reserved. Called by the
/// user-facing create/mkdir/rename entry points; the kernel's own parking
/// rename bypasses it (`abi_rename_raw`).
pub fn refuse_reserved(path: &str) -> Result<(), FsError> {
    let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    if is_reserved(name) {
        Err(FsError::Invalid)
    } else {
        Ok(())
    }
}
