//! In-memory ramfs: the writable filesystem used for `/tmp` and by the VFS
//! unit tests.
//!
//! A tree of [`Node`]s lives under one `spin::Mutex`; the root is inode 1.
//! Files keep their bytes in a `Vec<u8>`, directories keep an ordered child
//! list (so `readdir` is stable). Owner uid/gid and mode bits are stamped on
//! creation by the VFS, which is what the permission checks read back.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use super::vfs::{DirEntry, FileKind, Filesystem, FsError, Id, Meta, StatFs, S_IFDIR, S_IFREG};

mod capacity;

/// The root directory's inode. Inodes are allocated upward from here.
const ROOT_INO: u64 = 1;

/// Default cap on the file bytes one ramfs holds (4 MiB of the 16 MiB kernel
/// heap). `/tmp` is shared by every user and backed by that heap, so without a
/// cap one process could exhaust it and abort the kernel.
pub const DEFAULT_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Default cap on live nodes (files, directories and the root).
pub const DEFAULT_MAX_NODES: usize = 4096;

/// One node: a file with contents, or a directory with children.
struct Node {
    name: String,
    kind: FileKind,
    /// Permission bits only (type bits are added by [`Node::meta`]).
    mode: u16,
    uid: u32,
    gid: u32,
    data: Vec<u8>,
    children: Vec<u64>,
}

impl Node {
    fn meta(&self, ino: u64) -> Meta {
        let kind_bits = match self.kind {
            FileKind::File => S_IFREG,
            FileKind::Dir => S_IFDIR,
        };
        Meta {
            ino,
            mode: kind_bits | (self.mode & 0o7777),
            uid: self.uid,
            gid: self.gid,
            size: self.data.len() as u64,
            kind: self.kind,
        }
    }
}

struct Inner {
    next_ino: u64,
    nodes: BTreeMap<u64, Node>,
    /// Sum of every file's `data.len()`, so the overlay can enforce a byte cap
    /// without walking the tree.
    bytes: usize,
    /// Number of live nodes (including the root), for the overlay's node cap.
    live: usize,
    max_bytes: usize,
    max_nodes: usize,
}

/// An in-memory filesystem; see the module docs.
pub struct RamFs {
    inner: Mutex<Inner>,
}

impl RamFs {
    /// A fresh filesystem with only the root directory (`0755 root:root`) and
    /// the default caps ([`DEFAULT_MAX_BYTES`], [`DEFAULT_MAX_NODES`]).
    pub fn new() -> RamFs {
        RamFs::with_limits(DEFAULT_MAX_BYTES, DEFAULT_MAX_NODES)
    }

    /// A ramfs with explicit caps; writes and creations past them fail with
    /// [`FsError::NoSpace`]. Tests use tiny values.
    pub fn with_limits(max_bytes: usize, max_nodes: usize) -> RamFs {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            ROOT_INO,
            Node {
                name: String::from("/"),
                kind: FileKind::Dir,
                mode: 0o755,
                uid: 0,
                gid: 0,
                data: Vec::new(),
                children: Vec::new(),
            },
        );
        RamFs {
            inner: Mutex::new(Inner {
                next_ino: ROOT_INO + 1,
                nodes,
                bytes: 0,
                live: 1,
                max_bytes,
                max_nodes,
            }),
        }
    }

    /// Layer accounting: `(file data bytes, live nodes)`. Used by the copy-up
    /// overlay to enforce its heap cap and by tests to prove resources return
    /// to baseline.
    pub fn usage(&self) -> (usize, usize) {
        let inner = self.inner.lock();
        (inner.bytes, inner.live)
    }

    /// Resolve a relative path to an inode, walking components from the root.
    fn resolve(inner: &Inner, path: &str) -> Result<u64, FsError> {
        let mut ino = ROOT_INO;
        for part in path.split('/').filter(|part| !part.is_empty()) {
            let node = inner.nodes.get(&ino).ok_or(FsError::NotFound)?;
            if node.kind != FileKind::Dir {
                return Err(FsError::NotDir);
            }
            let child = node
                .children
                .iter()
                .find(|&&child| inner.nodes[&child].name == part)
                .copied();
            ino = child.ok_or(FsError::NotFound)?;
        }
        Ok(ino)
    }

    /// Split a path into `(parent inode, final name)`, requiring the parent to
    /// exist and be a directory. The final name must not name an existing
    /// child (`FsError::Exists` is handled by the callers).
    fn resolve_parent(inner: &Inner, path: &str) -> Result<(u64, String), FsError> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return Err(FsError::Exists); // cannot create or remove the root
        }
        let (parent, name) = match path.rsplit_once('/') {
            Some((parent, name)) => (parent, name),
            None => ("", path),
        };
        if name.is_empty() || name.len() > 255 {
            return Err(FsError::NameTooLong);
        }
        if name == "." || name == ".." {
            return Err(FsError::Invalid);
        }
        let parent_ino = Self::resolve(inner, parent)?;
        let parent_node = inner.nodes.get(&parent_ino).ok_or(FsError::NotFound)?;
        if parent_node.kind != FileKind::Dir {
            return Err(FsError::NotDir);
        }
        Ok((parent_ino, String::from(name)))
    }

    /// Find an existing child of `parent` by name.
    fn child(inner: &Inner, parent: u64, name: &str) -> Option<u64> {
        inner.nodes[&parent]
            .children
            .iter()
            .find(|&&child| inner.nodes[&child].name == name)
            .copied()
    }

    /// Whether one more node fits under the cap.
    fn check_node_room(inner: &Inner) -> Result<(), FsError> {
        if inner.live >= inner.max_nodes {
            return Err(FsError::NoSpace);
        }
        Ok(())
    }

    /// Resize file `ino` to `new_len` bytes, charging the difference against
    /// the byte cap first and reserving with `try_reserve_exact`, so running
    /// out of heap is `ENOSPC` rather than an allocation-failure abort. The
    /// accounting only moves once the allocation succeeded.
    fn resize_file(inner: &mut Inner, ino: u64, new_len: usize) -> Result<(), FsError> {
        let old_len = inner.nodes[&ino].data.len();
        let total = inner.bytes - old_len;
        match total.checked_add(new_len) {
            Some(next) if next <= inner.max_bytes => {}
            _ => return Err(FsError::NoSpace),
        }
        // INVARIANT: callers resolved `ino` under the same lock held here.
        let node = inner.nodes.get_mut(&ino).expect("resolved inode exists");
        if new_len > old_len {
            node.data
                .try_reserve_exact(new_len - old_len)
                .map_err(|_| FsError::NoSpace)?;
        }
        node.data.resize(new_len, 0); // zero-fills a sparse gap
        inner.bytes = total + new_len;
        Ok(())
    }

    /// Whether `target` is `root` or lies anywhere below it. The traversal
    /// stack grows fallibly: a nearly full heap is `NoSpace`, not an abort.
    fn is_within(inner: &Inner, root: u64, target: u64) -> Result<bool, FsError> {
        let mut stack: Vec<u64> = Vec::new();
        stack.try_reserve(1).map_err(|_| FsError::NoSpace)?;
        stack.push(root);
        while let Some(ino) = stack.pop() {
            if ino == target {
                return Ok(true);
            }
            let children = &inner.nodes[&ino].children;
            stack
                .try_reserve(children.len())
                .map_err(|_| FsError::NoSpace)?;
            stack.extend_from_slice(children);
        }
        Ok(false)
    }

    /// Create a node and link it into its parent.
    fn insert(
        inner: &mut Inner,
        parent: u64,
        name: String,
        kind: FileKind,
        mode: u16,
        owner: Id,
    ) -> Meta {
        let ino = inner.next_ino;
        inner.next_ino += 1;
        inner.nodes.insert(
            ino,
            Node {
                name,
                kind,
                mode,
                uid: owner.uid,
                gid: owner.gid,
                data: Vec::new(),
                children: Vec::new(),
            },
        );
        // INVARIANT: `parent` was resolved by the caller under the same
        // `inner` lock held here, and nothing else can remove it while we
        // hold that lock (single-threaded access to `inner`).
        inner
            .nodes
            .get_mut(&parent)
            .expect("parent exists")
            .children
            .push(ino);
        inner.live += 1;
        inner.nodes[&ino].meta(ino)
    }
}

impl Default for RamFs {
    fn default() -> Self {
        RamFs::new()
    }
}

impl Filesystem for RamFs {
    fn name(&self) -> &'static str {
        "ramfs"
    }

    fn statfs(&self) -> Result<StatFs, FsError> {
        Ok(self.capacity())
    }

    fn lookup(&self, path: &str) -> Result<Meta, FsError> {
        let inner = self.inner.lock();
        let ino = Self::resolve(&inner, path)?;
        Ok(inner.nodes[&ino].meta(ino))
    }

    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let inner = self.inner.lock();
        let ino = Self::resolve(&inner, path)?;
        let node = &inner.nodes[&ino];
        if node.kind != FileKind::File {
            return Err(FsError::IsDir);
        }
        if offset >= node.data.len() as u64 {
            return Ok(0);
        }
        let start = offset as usize;
        let count = (node.data.len() - start).min(buf.len());
        buf[..count].copy_from_slice(&node.data[start..start + count]);
        Ok(count)
    }

    fn write(&self, path: &str, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        let mut inner = self.inner.lock();
        let ino = Self::resolve(&inner, path)?;
        if inner.nodes[&ino].kind != FileKind::File {
            return Err(FsError::IsDir);
        }
        let end = offset
            .checked_add(data.len() as u64)
            .and_then(|end| usize::try_from(end).ok())
            .ok_or(FsError::NoSpace)?;
        if end > inner.nodes[&ino].data.len() {
            Self::resize_file(&mut inner, ino, end)?;
        }
        // INVARIANT: `ino` was just resolved above under this same lock, and
        // nothing else can remove it while we hold `inner`.
        let node = inner.nodes.get_mut(&ino).expect("resolved inode exists");
        node.data[offset as usize..end].copy_from_slice(data);
        Ok(data.len())
    }

    fn truncate(&self, path: &str, size: u64) -> Result<(), FsError> {
        let mut inner = self.inner.lock();
        let ino = Self::resolve(&inner, path)?;
        if inner.nodes[&ino].kind != FileKind::File {
            return Err(FsError::IsDir);
        }
        let size = usize::try_from(size).map_err(|_| FsError::NoSpace)?;
        Self::resize_file(&mut inner, ino, size)
    }

    fn create(&self, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError> {
        let mut inner = self.inner.lock();
        let (parent, name) = Self::resolve_parent(&inner, path)?;
        if Self::child(&inner, parent, &name).is_some() {
            return Err(FsError::Exists);
        }
        Self::check_node_room(&inner)?;
        Ok(Self::insert(
            &mut inner,
            parent,
            name,
            FileKind::File,
            mode,
            owner,
        ))
    }

    fn mkdir(&self, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError> {
        let mut inner = self.inner.lock();
        let (parent, name) = Self::resolve_parent(&inner, path)?;
        if Self::child(&inner, parent, &name).is_some() {
            return Err(FsError::Exists);
        }
        Self::check_node_room(&inner)?;
        Ok(Self::insert(
            &mut inner,
            parent,
            name,
            FileKind::Dir,
            mode,
            owner,
        ))
    }

    fn unlink(&self, path: &str) -> Result<(), FsError> {
        let mut inner = self.inner.lock();
        let (parent, name) = Self::resolve_parent(&inner, path)?;
        let ino = Self::child(&inner, parent, &name).ok_or(FsError::NotFound)?;
        if inner.nodes[&ino].kind == FileKind::Dir {
            return Err(FsError::IsDir); // directories need rmdir
        }
        // INVARIANT: `parent` was resolved above under this same lock; see
        // the note in `insert` for why it cannot have gone away since.
        inner
            .nodes
            .get_mut(&parent)
            .expect("parent exists")
            .children
            .retain(|&child| child != ino);
        let removed = inner.nodes.remove(&ino).expect("resolved inode exists");
        inner.bytes -= removed.data.len();
        inner.live -= 1;
        Ok(())
    }

    fn rmdir(&self, path: &str) -> Result<(), FsError> {
        let mut inner = self.inner.lock();
        let (parent, name) = Self::resolve_parent(&inner, path)?;
        let ino = Self::child(&inner, parent, &name).ok_or(FsError::NotFound)?;
        if inner.nodes[&ino].kind != FileKind::Dir {
            return Err(FsError::NotDir);
        }
        if !inner.nodes[&ino].children.is_empty() {
            return Err(FsError::NotEmpty);
        }
        // INVARIANT: `parent` was resolved above under this same lock; see
        // the note in `insert` for why it cannot have gone away since.
        inner
            .nodes
            .get_mut(&parent)
            .expect("parent exists")
            .children
            .retain(|&child| child != ino);
        inner.nodes.remove(&ino);
        inner.live -= 1;
        Ok(())
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        let mut inner = self.inner.lock();
        let (from_parent, from_name) = Self::resolve_parent(&inner, from)?;
        let source = Self::child(&inner, from_parent, &from_name).ok_or(FsError::NotFound)?;
        let (to_parent, to_name) = Self::resolve_parent(&inner, to)?;

        // Renaming a node onto itself (any two spellings of one path) is a
        // no-op, not a replace: removing the "existing" node would remove the
        // source.
        if Self::child(&inner, to_parent, &to_name) == Some(source) {
            return Ok(());
        }
        // A directory cannot move beneath itself: that would detach the
        // subtree into a cycle unreachable from the root.
        if inner.nodes[&source].kind == FileKind::Dir && Self::is_within(&inner, source, to_parent)?
        {
            return Err(FsError::Invalid);
        }

        // The destination may exist: a file replaces a file, a directory may
        // replace only an empty directory (POSIX would allow exactly this set).
        if let Some(existing) = Self::child(&inner, to_parent, &to_name) {
            let source_kind = inner.nodes[&source].kind;
            let existing_kind = inner.nodes[&existing].kind;
            match (source_kind, existing_kind) {
                (FileKind::File, FileKind::File) => {}
                (FileKind::Dir, FileKind::Dir) => {
                    if !inner.nodes[&existing].children.is_empty() {
                        return Err(FsError::NotEmpty);
                    }
                }
                (FileKind::File, FileKind::Dir) => return Err(FsError::IsDir),
                (FileKind::Dir, FileKind::File) => return Err(FsError::NotDir),
            }
            // INVARIANT: `to_parent`/`existing` were resolved above under
            // this same lock; see the note in `insert` for why they cannot
            // have gone away since.
            inner
                .nodes
                .get_mut(&to_parent)
                .expect("parent exists")
                .children
                .retain(|&child| child != existing);
            let removed = inner
                .nodes
                .remove(&existing)
                .expect("resolved inode exists");
            inner.bytes -= removed.data.len();
            inner.live -= 1;
        }

        // INVARIANT: `from_parent`/`source` were resolved above under this
        // same lock; see the note in `insert` for why they cannot have gone
        // away since.
        inner
            .nodes
            .get_mut(&from_parent)
            .expect("parent exists")
            .children
            .retain(|&child| child != source);
        inner.nodes.get_mut(&source).expect("source exists").name = to_name;
        inner
            .nodes
            .get_mut(&to_parent)
            .expect("parent exists")
            .children
            .push(source);
        Ok(())
    }

    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        let inner = self.inner.lock();
        let ino = Self::resolve(&inner, path)?;
        let node = &inner.nodes[&ino];
        if node.kind != FileKind::Dir {
            return Err(FsError::NotDir);
        }
        Ok(node
            .children
            .iter()
            .map(|&ino| {
                let child = &inner.nodes[&ino];
                DirEntry {
                    name: child.name.clone(),
                    ino,
                    kind: child.kind,
                }
            })
            .collect())
    }
}
