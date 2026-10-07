//! Attribute changes (`chmod`, `chown`, `utimensat`) through the Linux ABI
//! mount table, and the native `chmod` (syscall 32) through the native one.
//! Kept beside, not inside, the other `abi_*`/`vfs_*` helpers in `mod.rs` to
//! hold that file under the size limit.

use super::coherence::{abi_changed, native_changed, Change};
use super::vfs::{AttrRequest, FsError, Id, Meta};
use super::{abi_with, with};

/// Change an attribute of `path` as `id` (search needed on every ancestor).
pub fn abi_setattr(id: Id, path: &str, request: AttrRequest) -> Result<Meta, FsError> {
    let changed = abi_with(|vfs| vfs.setattr(id, path, request)).unwrap_or(Err(FsError::NotFound));
    abi_changed(path, Change::Content);
    changed
}

/// Change an attribute of a file `id` holds open at `path` (`fchmod`,
/// `fchown`, `futimens`): the name is not searched again, only the request's
/// ownership rule applies.
pub fn abi_setattr_open(id: Id, path: &str, request: AttrRequest) -> Result<Meta, FsError> {
    let changed =
        abi_with(|vfs| vfs.setattr_open(id, path, request)).unwrap_or(Err(FsError::NotFound));
    abi_changed(path, Change::Content);
    changed
}

/// Change an attribute of `path` as `id` through the native mount table: the
/// same [`Vfs::setattr`](super::vfs::Vfs::setattr) rules the Linux `chmod`
/// family gets (owner or root, read-only mounts refused first).
pub fn vfs_setattr(id: Id, path: &str, request: AttrRequest) -> Result<Meta, FsError> {
    let changed = with(|vfs| vfs.setattr(id, path, request)).unwrap_or(Err(FsError::NotFound));
    native_changed(path, Change::Content);
    changed
}
