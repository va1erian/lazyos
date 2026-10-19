//! Whole-mount operations on the [`Vfs`]: durability, capacity, and which mount
//! a path lands on. Split from `vfs.rs` because none of them touch a node, only
//! the filesystem that holds it, and to keep that file under the size limit.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::{Filesystem, FsError, Id, MountFlags, Path, StatFs, Vfs};

/// A flush resolved under the mount table's lock and run after it is
/// released ([`Vfs::flush_target`]): the filesystem outlives the table lock
/// because the target holds it.
pub struct FlushTarget {
    fs: Arc<dyn Filesystem>,
    rel: String,
    /// The whole filesystem, not just the file at `rel` (`syncfs`, `sync`).
    whole: bool,
}

impl FlushTarget {
    /// Run the flush. A regular file is flushed on its own where its backend
    /// can; a directory, or a backend with no nodes, flushes the filesystem.
    pub fn run(self) -> Result<(), FsError> {
        if self.whole {
            return self.fs.flush();
        }
        match self.fs.open_node(&self.rel) {
            Ok(Some(node)) => self.fs.flush_node(node),
            _ => self.fs.flush(),
        }
    }
}

/// Flush `filesystems` one after the other; one failing does not stop the
/// rest, and the first error is reported ([`Vfs::sync_all`] without the lock).
pub fn flush_all(filesystems: Vec<Arc<dyn Filesystem>>) -> Result<(), FsError> {
    let mut first = Ok(());
    for fs in filesystems {
        if let Err(error) = fs.flush() {
            first = first.and(Err(error));
        }
    }
    first
}

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

    /// Resolve the flush of `path` ([`Vfs::flush`]'s checks) without running
    /// it, so the caller can release the table's lock first: a flush writes
    /// to the device, and every other task's file call waits behind this lock
    /// meanwhile. `whole` flushes the filesystem (`syncfs`); otherwise the
    /// file alone, where the backend can ([`Filesystem::flush_node`]).
    pub fn flush_target(
        &mut self,
        id: Id,
        path: &str,
        whole: bool,
    ) -> Result<FlushTarget, FsError> {
        let path = Path::parse(path);
        self.check_path(id, &path, 0)?;
        let (mount, rel) = self.resolve_mount(&path)?;
        Ok(FlushTarget {
            fs: Arc::clone(&self.mounts[mount].fs),
            rel,
            whole,
        })
    }

    /// The mounted filesystems, for a flush after the lock is released.
    pub fn filesystems(&self) -> Vec<Arc<dyn Filesystem>> {
        self.mounts.iter().map(|mount| Arc::clone(&mount.fs)).collect()
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
    /// away, `fs::fuse`). A mount with another mount below it, or whose
    /// filesystem is mounted at another point too, is [`FsError::Invalid`];
    /// no mount there is [`FsError::NotFound`].
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
        // A filesystem also mounted elsewhere stays: the open-file registry
        // names its files by the first mount point (`canonical_path`), and
        // taking that name away would orphan every registered file.
        let fs = &self.mounts[index].fs;
        if self
            .mounts
            .iter()
            .enumerate()
            .any(|(other, mount)| other != index && alloc::sync::Arc::ptr_eq(&mount.fs, fs))
        {
            return Err(FsError::Invalid);
        }
        self.mounts.remove(index);
        self.dentry.clear();
        self.inodes.clear();
        self.stats.mounts = self.mounts.len();
        self.stats.invalidations += 1;
        Ok(())
    }

    /// The mount point of the mount holding `path`, judged lexically (the
    /// longest mount point that is a prefix of it).
    pub fn mount_point(&self, path: &str) -> Option<alloc::string::String> {
        let (mount, _) = self.resolve_mount(&Path::parse(path)).ok()?;
        Some(self.mounts[mount].point.to_path_string())
    }

    /// `path` as it is named at the first mount of the filesystem holding it:
    /// `/tmp/x` is `/transient/x` while one ramfs is mounted at both points.
    /// Anything keyed by path text (the open-file registry) uses this form, so
    /// two names of one file are one key. It assumes the two mount points have
    /// the same mounts below them (true for the one alias in use, `/tmp` and
    /// `/transient`; a mount under one name only would be misnamed).
    pub fn canonical_path(&self, path: &str) -> alloc::string::String {
        let path = Path::parse(path);
        let Ok((mount, rel)) = self.resolve_mount(&path) else {
            return path.to_path_string();
        };
        let base = self.mounts[self.cache_key(mount)].point.to_path_string();
        match (base.as_str(), rel.is_empty()) {
            (_, true) => base,
            ("/", false) => alloc::format!("/{rel}"),
            (_, false) => alloc::format!("{base}/{rel}"),
        }
    }

    /// The short name of the filesystem holding `path` (`"ext2 (rw)"`, ...).
    pub fn mount_fs_name(&self, path: &str) -> Option<&'static str> {
        let (mount, _) = self.resolve_mount(&Path::parse(path)).ok()?;
        Some(self.mounts[mount].fs.name())
    }
}
