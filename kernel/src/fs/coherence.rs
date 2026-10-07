//! Keeping the two mount tables' metadata caches coherent.
//!
//! In the configured layout the native and Linux ABI tables mount the same
//! volume instances, but each [`Vfs`] keeps its own dentry and inode cache. A
//! mutation through one table must drop what the other cached for the same
//! path, or the other side answers stale metadata: a native `chmod` that
//! tightens a file would still let a Linux `access`/`open` through, and an
//! ABI `rmdir` would leave the native table stat-ing a directory that is gone.
//!
//! Each helper here runs after the mutation, with the mutating table's lock
//! already released, so the two locks are never held together. In the legacy
//! layout the ABI root is an overlay over a different filesystem; dropping
//! cache entries that did not change there costs one re-read and nothing else.

use super::vfs::{Path, Vfs};
use super::{abi_with, with};

/// What a mutation changed about its path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Change {
    /// Contents or attributes (`write`, `truncate`, `chmod`, `chown`,
    /// `utimens`): the name still refers to the same inode.
    Content,
    /// The name itself (`create`, `mkdir`, `rmdir`, `unlink`, either side of
    /// a `rename`): it may now name another inode or none, and its parent's
    /// size, link count and times moved too.
    Name,
}

/// A native mutation changed `path`: drop the Linux ABI table's cache for it
/// (`accountsd`'s `/system/etc/passwd` view, U1, is rewritten natively and
/// read by Linux programs).
pub(super) fn native_changed(path: &str, change: Change) {
    abi_with(|vfs| apply(vfs, path, change));
}

/// A Linux ABI mutation changed `path`: drop the native table's cache for it.
pub(super) fn abi_changed(path: &str, change: Change) {
    with(|vfs| apply(vfs, path, change));
}

fn apply(vfs: &mut Vfs, path: &str, change: Change) {
    match change {
        Change::Content => vfs.forget(path),
        Change::Name => {
            vfs.invalidate(path);
            let parsed = Path::parse(path);
            if !parsed.is_root() {
                vfs.forget(&parsed.parent().to_path_string());
            }
        }
    }
}
