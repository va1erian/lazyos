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

use super::vfs::{DirEntry, FileKind, Filesystem, FsError, Id, Meta, S_IFDIR, S_IFREG};

/// The root directory's inode. Inodes are allocated upward from here.
const ROOT_INO: u64 = 1;

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
}

/// An in-memory filesystem; see the module docs.
pub struct RamFs {
    inner: Mutex<Inner>,
}

impl RamFs {
    /// A fresh filesystem with only the root directory (`0755 root:root`).
    pub fn new() -> RamFs {
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
            }),
        }
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
        let Some(end) = offset.checked_add(data.len() as u64) else {
            return Err(FsError::NoSpace);
        };
        // INVARIANT: `ino` was just resolved above under this same lock, and
        // nothing else can remove it while we hold `inner`.
        let node = inner.nodes.get_mut(&ino).expect("resolved inode exists");
        let end = end as usize;
        if end > node.data.len() {
            node.data.resize(end, 0); // sparse writes zero-fill the gap
        }
        node.data[offset as usize..end].copy_from_slice(data);
        Ok(data.len())
    }

    fn create(&self, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError> {
        let mut inner = self.inner.lock();
        let (parent, name) = Self::resolve_parent(&inner, path)?;
        if Self::child(&inner, parent, &name).is_some() {
            return Err(FsError::Exists);
        }
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
            return Err(FsError::IsDir); // no rmdir in this slice
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
        Ok(())
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        let mut inner = self.inner.lock();
        let (from_parent, from_name) = Self::resolve_parent(&inner, from)?;
        let source = Self::child(&inner, from_parent, &from_name).ok_or(FsError::NotFound)?;
        let (to_parent, to_name) = Self::resolve_parent(&inner, to)?;

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
            inner.nodes.remove(&existing);
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
