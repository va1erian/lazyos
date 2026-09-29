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

use crate::ipc::credentials;

/// Type and mode bits (Linux values); `mode` in [`Meta`] uses these.
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // masked by tests/diagnostics
pub const S_IFMT: u16 = 0o170000;
/// Regular file.
pub const S_IFREG: u16 = 0o100000;
/// Directory.
pub const S_IFDIR: u16 = 0o040000;
/// Sticky bit on a directory (see [`check_sticky`]).
pub const S_ISVTX: u16 = 0o1000;

/// Permission masks for [`check_access`], with the POSIX `R_OK`/`W_OK`/`X_OK`
/// values so `access(2)` can pass its mode straight through.
pub const READ: u8 = 4;
pub const WRITE: u8 = 2;
pub const EXECUTE: u8 = 1;

/// What kind of node an entry is. There is no symlink or device node yet: the
/// resolver is symlink-free and the ABI layer fabricates its device nodes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileKind {
    File,
    Dir,
}

/// Metadata for one node, as the trait and the caches carry it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Meta {
    /// Inode number within the mount. Not stable across mounts, so cache keys
    /// always pair it with the mount index.
    pub ino: u64,
    /// Full mode: type bits (`S_IF*`) plus `rwx` bits and suid/sgid/sticky.
    pub mode: u16,
    /// Owner uid, stamped from kernel credentials at creation.
    pub uid: u32,
    /// Owner gid, stamped from kernel credentials at creation.
    pub gid: u32,
    /// Size in bytes (directories report their serialized/entry size).
    pub size: u64,
    /// The node type, kept explicit so callers do not re-mask `mode`.
    pub kind: FileKind,
}

/// One directory entry: the name plus the target's inode and kind.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DirEntry {
    pub name: String,
    pub ino: u64,
    pub kind: FileKind,
}

/// A kernel-stamped `uid`/`gid` pair: the owner stamped on new nodes, or the
/// caller checked for access. Linux tasks keep their caps and label elsewhere
/// ([`crate::ipc::credentials`]); the VFS only needs the ids.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Id {
    pub uid: u32,
    pub gid: u32,
}

impl Id {
    /// The kernel/bring-up identity: uid 0, gid 0.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // used by tests/diagnostics
    pub const ROOT: Id = Id { uid: 0, gid: 0 };

    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // used by tests/diagnostics
    pub const fn new(uid: u32, gid: u32) -> Id {
        Id { uid, gid }
    }

    /// The current task's kernel-stamped credentials, read once per VFS call
    /// so a file cannot be reached with forged identity.
    pub fn current() -> Id {
        let cred = credentials::of(crate::task::current());
        Id {
            uid: cred.uid,
            gid: cred.gid,
        }
    }

    /// Root bypasses the permission bits (documented in [`check_access`]).
    pub const fn is_root(self) -> bool {
        self.uid == 0
    }
}

/// Errors shared by the VFS and every filesystem implementation. The Linux ABI
/// layer maps each variant to its errno; [`FsError::message`] is the friendly
/// kernel-side text (serial logs, test failures).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FsError {
    NotFound,
    Exists,
    NotDir,
    IsDir,
    NotEmpty,
    Access,
    ReadOnly,
    Invalid,
    NoSpace,
    NameTooLong,
    NotSupported,
}

impl FsError {
    /// Human-readable text for logs and diagnostics; not an errno.
    pub fn message(self) -> &'static str {
        match self {
            FsError::NotFound => "no such file or directory",
            FsError::Exists => "file already exists",
            FsError::NotDir => "not a directory",
            FsError::IsDir => "is a directory",
            FsError::NotEmpty => "directory not empty",
            FsError::Access => "permission denied",
            FsError::ReadOnly => "read-only filesystem",
            FsError::Invalid => "invalid argument",
            FsError::NoSpace => "no space left on device",
            FsError::NameTooLong => "file name too long",
            FsError::NotSupported => "operation not supported",
        }
    }
}

/// Check the owner/group/other bits of `meta` against `mask` ([`READ`],
/// [`WRITE`], and/or [`EXECUTE`]).
///
/// The actor matches the owner bits when uids are equal, the group bits when
/// gids are equal (there are no supplementary groups yet), and the other bits
/// otherwise. Root bypasses the bits entirely: `docs/security-model.md` section
/// 4.1 grants that bypass to the kernel-init profile only, and uid 0 is that
/// profile until sessions land.
pub fn check_access(meta: &Meta, id: Id, mask: u8) -> Result<(), FsError> {
    if mask == 0 || id.is_root() {
        return Ok(());
    }
    let bits = if id.uid == meta.uid {
        (meta.mode >> 6) & 0o7
    } else if id.gid == meta.gid {
        (meta.mode >> 3) & 0o7
    } else {
        meta.mode & 0o7
    };
    if u16::from(mask) & bits == u16::from(mask) {
        Ok(())
    } else {
        Err(FsError::Access)
    }
}

/// The sticky-bit rule for `unlink`/`rename` inside `dir` (mode `S_ISVTX`):
/// the actor must be root, the directory's owner, or the entry's owner.
pub fn check_sticky(dir: &Meta, entry: &Meta, id: Id) -> Result<(), FsError> {
    if dir.mode & S_ISVTX == 0 {
        return Ok(());
    }
    if id.is_root() || id.uid == dir.uid || id.uid == entry.uid {
        Ok(())
    } else {
        Err(FsError::Access)
    }
}

/// A normalized, absolute VFS path: `/` plus components with `.`/`..` already
/// folded. Symlinks are not followed (there are none); see the module docs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Path {
    /// Whether the raw input started with `/`. Relative paths resolve from the
    /// root too until per-process cwds exist.
    absolute: bool,
    parts: Vec<String>,
}

impl Path {
    /// Fold a raw path into components: empty and `.` components drop, `..`
    /// pops (never above the root), and repeated slashes collapse. An empty
    /// input becomes the root.
    pub fn parse(raw: &str) -> Path {
        let absolute = raw.starts_with('/');
        let mut parts = Vec::new();
        for part in raw.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                name => parts.push(String::from(name)),
            }
        }
        Path { absolute, parts }
    }

    /// Whether the raw input was absolute. Both forms resolve from the root
    /// today, so this is informational (and tested).
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // used by tests/diagnostics
    pub fn is_absolute(&self) -> bool {
        self.absolute
    }

    pub fn is_root(&self) -> bool {
        self.parts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.parts.len()
    }

    /// The final component, or `None` for the root.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // used by tests/diagnostics
    pub fn name(&self) -> Option<&str> {
        self.parts.last().map(String::as_str)
    }

    /// The containing directory (the root's parent is the root).
    pub fn parent(&self) -> Path {
        let len = self.parts.len().saturating_sub(1);
        Path {
            absolute: true,
            parts: self.parts[..len].to_vec(),
        }
    }

    /// Every proper ancestor directory, root first. The root itself has none,
    /// so a search-permission walk checks exactly the directories that lead to
    /// the node and never the node twice.
    pub fn ancestors(&self) -> Vec<Path> {
        if self.parts.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(self.parts.len());
        out.push(Path {
            absolute: true,
            parts: Vec::new(),
        });
        for depth in 1..self.parts.len() {
            out.push(Path {
                absolute: true,
                parts: self.parts[..depth].to_vec(),
            });
        }
        out
    }

    /// Whether `prefix` is this path or one of its ancestors.
    pub fn starts_with(&self, prefix: &Path) -> bool {
        prefix.parts.len() <= self.parts.len()
            && self.parts[..prefix.parts.len()] == prefix.parts[..]
    }

    /// The canonical string form: `/`, `/a`, `/a/b`, ...
    pub fn to_path_string(&self) -> String {
        let mut out = String::new();
        for part in &self.parts {
            out.push('/');
            out.push_str(part);
        }
        if out.is_empty() {
            out.push('/');
        }
        out
    }
}

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

    /// List a directory's entries (without `.`/`..`, which the ABI layer adds).
    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError>;
}

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
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // used by tests/diagnostics
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
