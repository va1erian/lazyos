//! VFS core (issue #98): a mount table, path resolution, inode/dentry caches,
//! and UNIX permission checks over a small [`Filesystem`] trait.
//!
//! # Resolution semantics
//!
//! Resolution is deliberately **symlink-free**: the trait has no symlink node
//! type yet, so a path is just a sequence of directory entries. This is the
//! "symlink-free first" slice from issue #98; symlinks land with a follow-up
//! and will add a resolution loop here (and a link-follow flag on the ops).
//!
//! [`Path`] folds `.` and `..` lexically before any lookup and clamps `..` at
//! the root, so `/..` and `/a/../..` both resolve to `/`. Kernel tasks have no
//! per-process cwd yet (`chdir` is a no-op and `getcwd` reports `/`), so a
//! relative path is rooted at `/`, exactly like the old whole-file reader did.
//!
//! Every mount is a `(mount point, Filesystem)` pair; resolution picks the
//! longest mount-point prefix, so `/tmp/notes` lands in the ramfs mounted at
//! `/tmp` while `/tmp2` stays on the root filesystem. `..` is folded *before*
//! mount lookup, so `/tmp/../HELLO.TXT` is the root volume's file (the lexical
//! rule Linux applies once it has resolved that far).
//!
//! # Permissions
//!
//! Every entry point takes the caller [`Id`] (the kernel-stamped `uid`/`gid`
//! from [`crate::ipc::credentials`]). Directories need execute (search) on each
//! component of a path; the final node needs the access the operation implies.
//! Root (`uid 0`) bypasses the bits, matching `docs/security-model.md` section
//! 4.1 (the bypass exists only inside the kernel-init profile). The sticky bit
//! on a directory restricts `unlink`/`rename` to the entry owner, the directory
//! owner, or root.
//!
//! # Caches
//!
//! A dentry cache maps `(mount, path)` to an inode number and an inode cache
//! maps `(mount, ino)` to [`Meta`]. Lookups hit the caches first; mutations
//! refresh or invalidate the affected paths, including cached descendants of a
//! removed or renamed directory, so a stale name or size cannot be read back.
//! [`Vfs::cache_stats`] exposes the counters so tests can see the caches work.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

mod filesystem;
mod meta;
mod mountops;
mod path;

pub use filesystem::Filesystem;
pub use meta::*;
pub use path::Path;

/// Cache counters, exposed through [`Vfs::cache_stats`] for tests and future
/// `/proc` reporting.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct CacheStats {
    /// Positive dentry lookups served from the cache.
    pub dentry_hits: u64,
    /// Lookups that had to go to the filesystem.
    pub dentry_misses: u64,
    /// Inode entries found for a cached dentry.
    pub inode_hits: u64,
    /// Inode entries that had to be re-read (miss or dropped dentry).
    pub inode_misses: u64,
    /// Number of invalidation passes (one per mutation).
    pub invalidations: u64,
    /// Mounts currently in the table.
    pub mounts: usize,
}

/// One mount: a normalized mount point plus the filesystem behind it.
struct Mount {
    point: Path,
    fs: Arc<dyn Filesystem>,
}

/// A cached directory entry: just the inode number; the metadata lives in the
/// inode cache so a path rename does not duplicate it.
struct Dentry {
    ino: u64,
}

/// The filesystem root: mount table, caches, and set-once operation surface.
/// The kernel holds one global instance ([`crate::fs`]); tests build their own
/// so permission and cache cases are isolated.
pub struct Vfs {
    mounts: Vec<Mount>,
    /// `(mount index, relative path)` -> inode number.
    dentry: BTreeMap<(usize, String), Dentry>,
    /// `(mount index, inode number)` -> metadata.
    inodes: BTreeMap<(usize, u64), Meta>,
    stats: CacheStats,
    /// Creation mask; see [`Vfs::set_umask`]. Global until per-task umasks
    /// move into the task struct.
    umask: u16,
}

impl Vfs {
    /// An empty VFS with no mounts (Linux's default umask is `0o022`).
    pub fn new() -> Vfs {
        Vfs {
            mounts: Vec::new(),
            dentry: BTreeMap::new(),
            inodes: BTreeMap::new(),
            stats: CacheStats::default(),
            umask: 0o022,
        }
    }

    /// Mount `fs` at `point`. A duplicate mount point is [`FsError::Exists`].
    pub fn mount(&mut self, point: &str, fs: Arc<dyn Filesystem>) -> Result<(), FsError> {
        let point = Path::parse(point);
        if self.mounts.iter().any(|mount| mount.point == point) {
            return Err(FsError::Exists);
        }
        self.mounts.push(Mount { point, fs });
        self.stats.mounts = self.mounts.len();
        Ok(())
    }

    /// Mount points in mount order, paired with the filesystem's short name.
    pub fn mounts(&self) -> Vec<(String, &'static str)> {
        self.mounts
            .iter()
            .map(|mount| (mount.point.to_path_string(), mount.fs.name()))
            .collect()
    }

    /// The current creation mask.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // used by tests/diagnostics
    pub fn umask(&self) -> u16 {
        self.umask
    }

    /// Replace the creation mask, returning the previous one (like `umask(2)`).
    pub fn set_umask(&mut self, umask: u16) -> u16 {
        let previous = self.umask;
        self.umask = umask & 0o777;
        previous
    }

    /// A snapshot of the cache counters.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // used by tests/diagnostics
    pub fn cache_stats(&self) -> CacheStats {
        self.stats
    }

    /// Drop the cached metadata for `path` (and, for a directory, everything
    /// cached below it). Mutations call this internally; it is public so a
    /// filesystem that changed behind the VFS's back can be re-read.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // used by tests/diagnostics
    pub fn invalidate(&mut self, path: &str) {
        let path = Path::parse(path);
        if let Ok((mount, rel)) = self.resolve_mount(&path) {
            self.invalidate_mount_path(mount, &rel);
        }
    }

    /// Metadata for `path`, checking search on every ancestor directory. The
    /// final node needs no access bit (`stat(2)` semantics).
    pub fn stat(&mut self, id: Id, path: &str) -> Result<Meta, FsError> {
        let path = Path::parse(path);
        self.check_path(id, &path, 0)
    }

    /// Check `mask` on `path` itself (ancestors always need search). Used by
    /// `openat` for write access and by `access(2)`.
    pub fn check(&mut self, id: Id, path: &str, mask: u8) -> Result<Meta, FsError> {
        let path = Path::parse(path);
        self.check_path(id, &path, mask)
    }

    /// Read a whole file (`offset`-based reads use [`Vfs::read`]).
    pub fn read_file(&mut self, id: Id, path: &str) -> Result<Vec<u8>, FsError> {
        let path = Path::parse(path);
        let meta = self.check_path(id, &path, READ)?;
        if meta.kind != FileKind::File {
            return Err(FsError::IsDir);
        }
        let (mount, rel) = self.resolve_mount(&path)?;
        let fs = Arc::clone(&self.mounts[mount].fs);
        // One call for the whole file: a filesystem that has to walk a chain
        // (FAT) reads sequentially instead of re-seeking per chunk. If it
        // returns short, keep reading at the new EOF until the size is met.
        let mut data = super::fallible::zeroed(meta.size)?;
        let mut filled = fs.read(&rel, 0, &mut data)?;
        let mut chunk = [0u8; 4096];
        while (filled as u64) < meta.size {
            let read = fs.read(&rel, filled as u64, &mut chunk)?;
            if read == 0 {
                break;
            }
            let room = data.len() - filled;
            let copy = read.min(room);
            data[filled..filled + copy].copy_from_slice(&chunk[..copy]);
            filled += copy;
            if read > room {
                data.extend_from_slice(&chunk[room..read]);
                filled += read - room;
            }
        }
        data.truncate(filled);
        Ok(data)
    }

    /// Read up to `buf.len()` bytes from `path` at `offset`.
    pub fn read(
        &mut self,
        id: Id,
        path: &str,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, FsError> {
        let path = Path::parse(path);
        let meta = self.check_path(id, &path, READ)?;
        if meta.kind != FileKind::File {
            return Err(FsError::IsDir);
        }
        let (mount, rel) = self.resolve_mount(&path)?;
        self.mounts[mount].fs.read(&rel, offset, buf)
    }

    /// Write `data` to `path` at `offset`, refreshing the cached size.
    pub fn write(
        &mut self,
        id: Id,
        path: &str,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, FsError> {
        let path = Path::parse(path);
        let meta = self.check_path(id, &path, WRITE)?;
        if meta.kind != FileKind::File {
            return Err(FsError::IsDir);
        }
        let (mount, rel) = self.resolve_mount(&path)?;
        let fs = Arc::clone(&self.mounts[mount].fs);
        let written = fs.write(&rel, offset, data)?;
        if let Ok(updated) = fs.stat(&rel) {
            self.insert_cache(mount, &rel, updated);
        }
        Ok(written)
    }

    /// Truncate a regular file to `size` bytes, refreshing the cached size.
    pub fn truncate(&mut self, id: Id, path: &str, size: u64) -> Result<(), FsError> {
        let path = Path::parse(path);
        let meta = self.check_path(id, &path, WRITE)?;
        if meta.kind != FileKind::File {
            return Err(FsError::IsDir);
        }
        let (mount, rel) = self.resolve_mount(&path)?;
        let fs = Arc::clone(&self.mounts[mount].fs);
        fs.truncate(&rel, size)?;
        if let Ok(updated) = fs.stat(&rel) {
            self.insert_cache(mount, &rel, updated);
        }
        Ok(())
    }

    /// Create a regular file, stamping `owner` and applying the umask.
    pub fn create(&mut self, id: Id, path: &str, mode: u16) -> Result<Meta, FsError> {
        let path = Path::parse(path);
        if path.is_root() {
            return Err(FsError::IsDir);
        }
        self.check_path(id, &path.parent(), WRITE | EXECUTE)?;
        let mode = mode & !self.umask & 0o7777;
        let (mount, rel) = self.resolve_mount(&path)?;
        let meta = self.mounts[mount].fs.create(&rel, mode, id)?;
        self.insert_cache(mount, &rel, meta);
        Ok(meta)
    }

    /// Create a directory, stamping `owner` and applying the umask.
    pub fn mkdir(&mut self, id: Id, path: &str, mode: u16) -> Result<Meta, FsError> {
        let path = Path::parse(path);
        if path.is_root() {
            return Err(FsError::Exists);
        }
        self.check_path(id, &path.parent(), WRITE | EXECUTE)?;
        let mode = mode & !self.umask & 0o7777;
        let (mount, rel) = self.resolve_mount(&path)?;
        let meta = self.mounts[mount].fs.mkdir(&rel, mode, id)?;
        self.insert_cache(mount, &rel, meta);
        Ok(meta)
    }

    /// Remove a regular file. The parent needs write permission and the sticky
    /// bit protects entries in shared directories.
    pub fn unlink(&mut self, id: Id, path: &str) -> Result<(), FsError> {
        let path = Path::parse(path);
        if path.is_root() {
            return Err(FsError::Access);
        }
        let dir = self.check_path(id, &path.parent(), WRITE | EXECUTE)?;
        let target = self.stat_path(&path)?;
        if target.kind == FileKind::Dir {
            return Err(FsError::IsDir);
        }
        check_sticky(&dir, &target, id)?;
        let (mount, rel) = self.resolve_mount(&path)?;
        self.mounts[mount].fs.unlink(&rel)?;
        self.invalidate_mount_path(mount, &rel);
        Ok(())
    }

    /// Remove an empty directory. The parent needs write permission and the
    /// sticky bit protects entries in shared directories.
    pub fn rmdir(&mut self, id: Id, path: &str) -> Result<(), FsError> {
        let path = Path::parse(path);
        if path.is_root() {
            return Err(FsError::Access);
        }
        let dir = self.check_path(id, &path.parent(), WRITE | EXECUTE)?;
        let target = self.stat_path(&path)?;
        if target.kind != FileKind::Dir {
            return Err(FsError::NotDir);
        }
        check_sticky(&dir, &target, id)?;
        let (mount, rel) = self.resolve_mount(&path)?;
        self.mounts[mount].fs.rmdir(&rel)?;
        self.invalidate_mount_path(mount, &rel);
        Ok(())
    }

    /// Rename `from` to `to` within one mount. Both parent directories need
    /// write permission and the sticky bit applies to both sides.
    pub fn rename(&mut self, id: Id, from: &str, to: &str) -> Result<(), FsError> {
        let from = Path::parse(from);
        let to = Path::parse(to);
        if from.is_root() || to.is_root() {
            return Err(FsError::Access); // mount roots do not move
        }
        let from_dir = self.check_path(id, &from.parent(), WRITE | EXECUTE)?;
        let to_dir = self.check_path(id, &to.parent(), WRITE | EXECUTE)?;
        let target = self.stat_path(&from)?;
        if from.parts == to.parts {
            return Ok(()); // POSIX: onto itself is a no-op
        }
        if to.starts_with(&from) {
            return Err(FsError::Invalid); // would orphan the subtree
        }
        check_sticky(&from_dir, &target, id)?;
        if let Ok(existing) = self.stat_path(&to) {
            check_sticky(&to_dir, &existing, id)?;
        }
        let (from_mount, from_rel) = self.resolve_mount(&from)?;
        let (to_mount, to_rel) = self.resolve_mount(&to)?;
        if from_mount != to_mount {
            return Err(FsError::NotSupported); // no cross-mount rename yet
        }
        self.mounts[from_mount].fs.rename(&from_rel, &to_rel)?;
        self.invalidate_mount_path(from_mount, &from_rel);
        self.invalidate_mount_path(from_mount, &to_rel);
        Ok(())
    }

    /// List a directory's entries (`.`/`..` are the ABI layer's job).
    pub fn readdir(&mut self, id: Id, path: &str) -> Result<Vec<DirEntry>, FsError> {
        let path = Path::parse(path);
        let meta = self.check_path(id, &path, READ)?;
        if meta.kind != FileKind::Dir {
            return Err(FsError::NotDir);
        }
        let (mount, rel) = self.resolve_mount(&path)?;
        self.mounts[mount].fs.readdir(&rel)
    }

    /// Walk ancestors for search and the path itself for `mask`.
    fn check_path(&mut self, id: Id, path: &Path, mask: u8) -> Result<Meta, FsError> {
        for ancestor in path.ancestors() {
            let meta = self.stat_path(&ancestor)?;
            if meta.kind != FileKind::Dir {
                return Err(FsError::NotDir);
            }
            check_access(&meta, id, EXECUTE)?;
        }
        let meta = self.stat_path(path)?;
        if mask != 0 {
            check_access(&meta, id, mask)?;
        }
        Ok(meta)
    }

    /// Cache-aware metadata lookup for an absolute path.
    fn stat_path(&mut self, path: &Path) -> Result<Meta, FsError> {
        let (mount, rel) = self.resolve_mount(path)?;
        if let Some(dentry) = self.dentry.get(&(mount, rel.clone())) {
            self.stats.dentry_hits += 1;
            if let Some(meta) = self.inodes.get(&(mount, dentry.ino)) {
                self.stats.inode_hits += 1;
                return Ok(*meta);
            }
        } else {
            self.stats.dentry_misses += 1;
        }
        self.stats.inode_misses += 1;
        let meta = self.mounts[mount].fs.stat(&rel)?;
        self.insert_cache(mount, &rel, meta);
        Ok(meta)
    }

    /// Resolve a path to `(mount index, path within that filesystem)`, picking
    /// the longest matching mount point.
    fn resolve_mount(&self, path: &Path) -> Result<(usize, String), FsError> {
        let mut best: Option<usize> = None;
        for (index, mount) in self.mounts.iter().enumerate() {
            if path.starts_with(&mount.point)
                && best.is_none_or(|b| self.mounts[b].point.len() < mount.point.len())
            {
                best = Some(index);
            }
        }
        let index = best.ok_or(FsError::NotFound)?;
        let mount = &self.mounts[index];
        let rel = path.parts[mount.point.len()..].join("/");
        Ok((index, rel))
    }

    /// Record a fresh metadata pair in both caches.
    fn insert_cache(&mut self, mount: usize, rel: &str, meta: Meta) {
        self.inodes.insert((mount, meta.ino), meta);
        self.dentry
            .insert((mount, String::from(rel)), Dentry { ino: meta.ino });
    }

    /// Invalidate a relative path within one mount, its inode, and any cached
    /// descendants when it names an inode that other entries still point at.
    fn invalidate_mount_path(&mut self, mount: usize, rel: &str) {
        let ino = self
            .dentry
            .remove(&(mount, String::from(rel)))
            .map(|dentry| dentry.ino);
        if let Some(ino) = ino {
            self.inodes.remove(&(mount, ino));
            self.dentry
                .retain(|(cached_mount, _), entry| *cached_mount != mount || entry.ino != ino);
        }
        if !rel.is_empty() {
            let prefix = alloc::format!("{rel}/");
            let stale: Vec<(usize, String)> = self
                .dentry
                .keys()
                .filter(|(cached_mount, cached_rel)| {
                    *cached_mount == mount && cached_rel.starts_with(&prefix)
                })
                .cloned()
                .collect();
            for key in stale {
                if let Some(entry) = self.dentry.remove(&key) {
                    self.inodes.remove(&(mount, entry.ino));
                }
            }
        }
        self.stats.invalidations += 1;
    }
}

impl Default for Vfs {
    fn default() -> Self {
        Vfs::new()
    }
}
