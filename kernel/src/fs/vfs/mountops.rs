//! Whole-mount operations on the [`Vfs`]: durability, capacity, and which mount
//! a path lands on. Split from `vfs.rs` because none of them touch a node, only
//! the filesystem that holds it, and to keep that file under the size limit.

use alloc::string::String;

use super::{FsError, Id, Path, StatFs, Vfs};

impl Vfs {
    /// Flush the filesystem holding `path` to stable storage (`fsync(2)`).
    ///
    /// The path must resolve (search permission on every ancestor, as with any
    /// other VFS call) but needs no read or write bit: flushing is not reading
    /// or writing the file's data.
    pub fn flush(&mut self, id: Id, path: &str) -> Result<(), FsError> {
        let path = Path::parse(path);
        self.check_path(id, &path, 0)?;
        let (mount, _) = self.resolve_mount(&path)?;
        self.mounts[mount].fs.flush()
    }

    /// Flush every mounted filesystem (`sync(2)`, and the shutdown path).
    /// One filesystem failing does not stop the rest from being flushed; the
    /// first error is reported.
    pub fn sync_all(&self) -> Result<(), FsError> {
        let mut first = Ok(());
        for mount in &self.mounts {
            if let Err(error) = mount.fs.flush() {
                first = first.and(Err(error));
            }
        }
        first
    }

    /// Capacity of the filesystem holding `path` (`statfs(2)`). Like
    /// [`Vfs::flush`], the path only has to resolve.
    pub fn statfs(&mut self, id: Id, path: &str) -> Result<StatFs, FsError> {
        let path = Path::parse(path);
        self.check_path(id, &path, 0)?;
        let (mount, _) = self.resolve_mount(&path)?;
        self.mounts[mount].fs.statfs()
    }

    /// The mount point (`"/"`, `"/data"`, ...) whose filesystem holds `path`,
    /// judged lexically: the path need not exist. Callers use it to tell a
    /// path on the copy-up root from one on a mount of its own.
    pub fn mount_point(&self, path: &str) -> Option<String> {
        let (mount, _) = self.resolve_mount(&Path::parse(path)).ok()?;
        Some(self.mounts[mount].point.to_path_string())
    }
}
