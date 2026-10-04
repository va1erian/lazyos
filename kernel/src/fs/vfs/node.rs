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

use super::{FileKind, Filesystem, FsError, Id, Meta, Path, Vfs};

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

    pub fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        self.fs.read_node(self.id, offset, buf)
    }

    pub fn write(&self, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        if !self.writable {
            return Err(FsError::ReadOnly);
        }
        self.fs.write_node(self.id, offset, data)
    }

    pub fn stat(&self) -> Result<Meta, FsError> {
        self.fs.stat_node(self.id)
    }
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

    /// Record `meta` as the current metadata of `path` (after a write through
    /// a node), so a later `stat` by path does not answer the old size.
    pub fn refresh(&mut self, path: &str, meta: Meta) {
        if let Ok((mount, rel)) = self.resolve_mount(&Path::parse(path)) {
            self.insert_cache(mount, &rel, meta);
        }
    }
}
