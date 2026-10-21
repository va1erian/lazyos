//! Open files as nodes of their filesystem (docs/performance-plan.md P5).
//!
//! A descriptor that named its file by path paid, on every read, the mount
//! lookup, a permission walk over every ancestor and the backend's own path
//! resolution from the root. A [`Node`] is resolved once, when the file is
//! opened (and checked for access then, as POSIX decides access at `open`),
//! and reads and writes go straight to the backend by its [`NodeId`]. A
//! backend without nodes answers `None` and the caller keeps the path.
//!
//! The mount table caches metadata by path; a write through a node bypasses
//! it, so the writer hands the new metadata back with [`Vfs::refresh`].

use alloc::sync::Arc;

use super::{FileKind, Filesystem, FsError, Id, Meta, Path, StatFs, Vfs};

/// A file as its backend names it: an inode number and the generation that
/// tells this file from a later one reusing the number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeId {
    pub ino: u64,
    pub generation: u32,
}

/// A regular file opened by node, with the filesystem it lives on.
#[derive(Clone)]
pub struct Node {
    fs: Arc<dyn Filesystem>,
    id: NodeId,
    /// The mount allows writes (a read-only mount refuses them here, as
    /// [`Vfs::write`] does by path).
    writable: bool,
}

impl Node {
    pub fn id(&self) -> NodeId {
        self.id
    }

    /// Whether `other` is the same file: the same filesystem instance (a
    /// filesystem mounted at two points is one) and the same node. Two names
    /// of one file (hard links) open as nodes that are the same file.
    pub fn same_file(&self, other: &Node) -> bool {
        self.fs_key() == other.fs_key() && self.id == other.id
    }

    /// Whether this node is its file ([`Filesystem::stable_nodes`]): the
    /// open-file registry knows the file by it rather than by its name.
    pub fn stable(&self) -> bool {
        self.fs.stable_nodes()
    }

    /// The filesystem instance this node lives on, as a number that tells
    /// filesystems apart ([`Vfs::identity`] answers the same for a path).
    pub fn fs_key(&self) -> usize {
        fs_key(&self.fs)
    }

    pub fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        self.fs.read_node(self.id, offset, buf)
    }

    /// Whether the processes running this file may share its pages
    /// ([`Filesystem::image_shareable`]).
    pub fn image_shareable(&self) -> bool {
        self.fs.image_shareable()
    }

    pub fn write(&self, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        if !self.writable {
            return Err(FsError::ReadOnly);
        }
        self.fs.write_node(self.id, offset, data)
    }

    /// [`Filesystem::flush_page`]: one page of a mapped file, cut at the
    /// size at write time. `None`: busy or unsupported without waiting.
    pub fn flush_page(
        &self,
        offset: u64,
        data: &[u8],
        wait: bool,
    ) -> Option<Result<usize, FsError>> {
        if !self.writable {
            return Some(Err(FsError::ReadOnly));
        }
        self.fs.flush_page(self.id, offset, data, wait)
    }

    pub fn stat(&self) -> Result<Meta, FsError> {
        self.fs.stat_node(self.id)
    }

    pub fn truncate(&self, size: u64) -> Result<(), FsError> {
        self.fs.truncate_node(self.id, size)
    }

    /// Make this file durable (`fsync`) as far as its filesystem can without
    /// flushing the rest ([`Filesystem::flush_node`]), even after the file's
    /// name was taken by another file ([`super::openfile`]).
    pub fn flush(&self) -> Result<(), FsError> {
        self.fs.flush_node(self.id)
    }

    /// Flush the whole filesystem this file lives on (`syncfs`).
    pub fn flush_fs(&self) -> Result<(), FsError> {
        self.fs.flush()
    }

    /// Capacity of the filesystem this file lives on (`statfs`).
    pub fn statfs(&self) -> Result<StatFs, FsError> {
        self.fs.statfs()
    }
}

/// The address of a filesystem instance, without its vtable (two `Arc`s of
/// one instance may carry different vtable pointers).
fn fs_key(fs: &Arc<dyn Filesystem>) -> usize {
    Arc::as_ptr(fs) as *const () as usize
}

impl Vfs {
    /// Open the regular file at `path` as a [`Node`] after checking `mask`
    /// for `id` (and search on every ancestor). `Ok(None)` when its backend
    /// has no nodes.
    pub fn open_node(&mut self, id: Id, path: &str, mask: u8) -> Result<Option<Node>, FsError> {
        let path = Path::parse(path);
        let meta = self.check_path(id, &path, mask)?;
        if meta.kind != FileKind::File {
            return Err(FsError::IsDir);
        }
        let (mount, rel) = self.resolve_mount(&path)?;
        let fs = Arc::clone(&self.mounts[mount].fs);
        let writable = !self.mounts[mount].flags.ro;
        Ok(fs.open_node(&rel)?.map(|id| Node { fs, id, writable }))
    }

    /// What `path` names, cheaply: its filesystem instance (as
    /// [`Node::fs_key`] numbers it) and its metadata, from the caches when
    /// they hold it. No permission is checked: the open-file registry asks
    /// which open file a name is (`openfile`), on the caller's behalf only
    /// after the caller's own checks.
    pub fn identity(&mut self, path: &str) -> Result<(usize, Meta), FsError> {
        let path = Path::parse(path);
        let meta = self.stat_path(&path)?;
        let (mount, _) = self.resolve_mount(&path)?;
        Ok((fs_key(&self.mounts[mount].fs), meta))
    }

    /// The filesystem instance holding `path` ([`Node::fs_key`]) and whether
    /// its nodes are its files ([`Filesystem::stable_nodes`]).
    pub fn fs_of(&self, path: &str) -> Option<(usize, bool)> {
        let (mount, _) = self.resolve_mount(&Path::parse(path)).ok()?;
        let fs = &self.mounts[mount].fs;
        Some((fs_key(fs), fs.stable_nodes()))
    }

    /// Record `meta` as the current metadata of `path` (after a write through
    /// a node), so a later `stat` by path does not answer the old size.
    pub fn refresh(&mut self, path: &str, meta: Meta) {
        if let Ok((mount, rel)) = self.resolve_mount(&Path::parse(path)) {
            self.insert_cache(mount, &rel, meta);
        }
    }
}
