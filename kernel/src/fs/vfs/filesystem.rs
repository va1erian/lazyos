//! The [`Filesystem`] trait mounted filesystems implement.

use super::{DirEntry, FsError, Id, Meta, NodeId, SetAttr, StatFs};
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

    /// Resolve the regular file at `path` once for the `*_node` calls below
    /// (`vfs/node.rs`). The default has no nodes: `Ok(None)`, and callers
    /// keep using the path.
    fn open_node(&self, _path: &str) -> Result<Option<NodeId>, FsError> {
        Ok(None)
    }

    /// [`Filesystem::read`] by node. A node whose file is gone (deleted, or
    /// its inode reused) is [`FsError::NotFound`].
    fn read_node(&self, _node: NodeId, _offset: u64, _buf: &mut [u8]) -> Result<usize, FsError> {
        Err(FsError::NotSupported)
    }

    /// [`Filesystem::write`] by node.
    fn write_node(&self, _node: NodeId, _offset: u64, _data: &[u8]) -> Result<usize, FsError> {
        Err(FsError::NotSupported)
    }

    /// Metadata by node.
    fn stat_node(&self, _node: NodeId) -> Result<Meta, FsError> {
        Err(FsError::NotSupported)
    }

    /// Truncate (or zero-extend) a regular file to `size` bytes. Backends that
    /// do not implement it answer [`FsError::NotSupported`].
    fn truncate(&self, _path: &str, _size: u64) -> Result<(), FsError> {
        Err(FsError::NotSupported)
    }

    /// Apply an attribute change (`chmod`, `chown`, `utimensat`) and return the
    /// node's metadata afterwards. `attr` is already authorized by the VFS;
    /// the backend applies every selected field or, on failure, none. The
    /// default answers [`FsError::NotSupported`]; FAT answers
    /// [`FsError::ReadOnly`], and ext2, ramfs and the overlay implement it.
    fn setattr(&self, _path: &str, _attr: &SetAttr) -> Result<Meta, FsError> {
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
    /// ext2 writes its block cache back, flushes the device and marks the
    /// volume clean. `confd`'s passthrough
    /// `fsync` reaches this through [`Vfs::flush`].
    fn flush(&self) -> Result<(), FsError> {
        Ok(())
    }

    /// Write cached dirty data back without `flush`'s durability point (the
    /// volume is not marked clean): the periodic flusher, `fs/flusher.rs`.
    /// With `pressure`, also give back clean cached memory. The default has
    /// nothing cached.
    fn writeback(&self, pressure: bool) -> Result<(), FsError> {
        let _ = pressure;
        Ok(())
    }

    /// Capacity and free space (`statfs(2)`). Backends with nothing sensible to
    /// report keep the default [`FsError::NotSupported`].
    fn statfs(&self) -> Result<StatFs, FsError> {
        Err(FsError::NotSupported)
    }

    /// List a directory's entries (without `.`/`..`, which the ABI layer adds).
    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError>;
}
