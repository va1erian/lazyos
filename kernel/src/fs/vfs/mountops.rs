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

    /// Write back every mount's cached dirty data without marking anything
    /// clean (the periodic flusher). Like [`Vfs::sync_all`], one failure does
    /// not stop the rest; the first is reported.
    pub fn writeback_all(&self, pressure: bool) -> Result<(), FsError> {
        let mut first = Ok(());
        for mount in &self.mounts {
            if let Err(error) = mount.fs.writeback(pressure) {
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

    /// Remove the mount at exactly `point` (a user-space filesystem going
    /// away, `fs::fuse`). A mount with another mount below it is
    /// [`FsError::Invalid`]; no mount there is [`FsError::NotFound`].
    ///
    /// The caches key on mount indices, which shift, so they are dropped
    /// whole: unmounting is rare, and a cold cache only costs lookups.
    /// Open nodes keep the filesystem alive and keep failing or working as
    /// it decides; the table only forgets the path.
    pub fn unmount(&mut self, point: &str) -> Result<(), FsError> {
        let point = Path::parse(point);
        let index = self
            .mounts
            .iter()
            .position(|mount| mount.point == point)
            .ok_or(FsError::NotFound)?;
        let nested = self
            .mounts
            .iter()
            .any(|mount| mount.point.len() > point.len() && mount.point.starts_with(&point));
        if nested || point.is_root() {
            return Err(FsError::Invalid);
        }
        self.mounts.remove(index);
        self.dentry.clear();
        self.inodes.clear();
        self.stats.mounts = self.mounts.len();
        self.stats.invalidations += 1;
        Ok(())
    }

    /// The short name of the filesystem holding `path` (`"ext2 (rw)"`, ...).
    pub fn mount_fs_name(&self, path: &str) -> Option<&'static str> {
        let (mount, _) = self.resolve_mount(&Path::parse(path)).ok()?;
        Some(self.mounts[mount].fs.name())
    }
}
