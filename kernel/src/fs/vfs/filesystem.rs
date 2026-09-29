//! The [`Filesystem`] trait mounted filesystems implement.

use super::{DirEntry, FsError, Id, Meta};
use alloc::vec::Vec;

/// The filesystem implementations the VFS can mount. Methods take paths
/// relative to the filesystem's root (`""` is the root itself); the VFS checks
/// permissions and caches metadata *before* calling in, so an implementation
/// only enforces what is intrinsic to it being read-only (e.g. FAT returning
/// [`FsError::ReadOnly`]).
///
/// `lookup` resolves a path to its metadata; `stat` defaults to it, and a
/// filesystem may override `stat` if lookup is cheaper or lazier.
pub trait Filesystem: Send + Sync {
    /// Short name for diagnostics (`"ramfs"`, `"fat16 (ro)"`).
    fn name(&self) -> &'static str;

    /// Resolve `path` within this filesystem (no permission checks).
    fn lookup(&self, path: &str) -> Result<Meta, FsError>;

    /// Metadata for `path`; defaults to [`Filesystem::lookup`].
    fn stat(&self, path: &str) -> Result<Meta, FsError> {
        self.lookup(path)
    }

    /// Read up to `buf.len()` bytes at `offset`; returns the count, `0` at EOF.
    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, FsError>;

    /// Write `data` at `offset`, extending the file; returns the count.
    fn write(&self, path: &str, offset: u64, data: &[u8]) -> Result<usize, FsError>;

    /// Truncate (or zero-extend) a regular file to `size` bytes. Backends that
    /// do not implement it answer [`FsError::NotSupported`].
    fn truncate(&self, _path: &str, _size: u64) -> Result<(), FsError> {
        Err(FsError::NotSupported)
    }

    /// Create a regular file with `mode` (already masked by the umask).
    fn create(&self, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError>;

    /// Create a directory with `mode` (already masked by the umask).
    fn mkdir(&self, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError>;

    /// Remove a regular file.
    fn unlink(&self, path: &str) -> Result<(), FsError>;

    /// Remove an empty directory. The default answers
    /// [`FsError::NotSupported`]; the read-only FAT driver overrides it with
    /// [`FsError::ReadOnly`], and ramfs/overlay implement it.
    fn rmdir(&self, _path: &str) -> Result<(), FsError> {
        Err(FsError::NotSupported)
    }

    /// Rename/move a node within this filesystem.
    fn rename(&self, from: &str, to: &str) -> Result<(), FsError>;

    /// Flush this filesystem's pending writes to stable storage.
    ///
    /// The default is a no-op for in-memory backends (ramfs, the overlay);
    /// ext2 hands its device cache to the block layer. `regd`'s passthrough
    /// `fsync` reaches this through [`Vfs::flush`].
    fn flush(&self) -> Result<(), FsError> {
        Ok(())
    }

    /// List a directory's entries (without `.`/`..`, which the ABI layer adds).
    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError>;
}
