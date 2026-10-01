//! Whole-mount operations on the [`Vfs`]: durability, capacity, and which mount
//! a path lands on. Split from `vfs.rs` because none of them touch a node, only
//! the filesystem that holds it, and to keep that file under the size limit.

use super::{FsError, Id, MountFlags, Path, StatFs, Vfs};

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

    /// The flags of the mount holding `path`, judged lexically like
    /// [`Vfs::mount_point`]. A path no mount covers has no restrictions.
    pub fn mount_flags(&self, path: &str) -> MountFlags {
        self.resolve_mount(&Path::parse(path)).map_or_else(
            |_| MountFlags::default(),
            |(mount, _)| self.mounts[mount].flags,
        )
    }

    /// The short name of the filesystem holding `path` (`"ext2 (rw)"`, ...).
    pub fn mount_fs_name(&self, path: &str) -> Option<&'static str> {
        let (mount, _) = self.resolve_mount(&Path::parse(path)).ok()?;
        Some(self.mounts[mount].fs.name())
    }
}
