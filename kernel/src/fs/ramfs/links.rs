//! Names in the ramfs tree: hard links, dropping a name, and rename.
//!
//! A file has one [`Entry`] per name and a link count equal to their number;
//! it is freed with its last name. The node's own charge (`space.rs`) covers
//! one name, the entry whose `extra` is `None`; every further name made by
//! `link` is charged, as a node of the filesystem's cap, to whoever made it,
//! so the per-owner half of the caps (issue #265) bounds names too. When the
//! covered name goes and others remain, one of them takes its place and its
//! charge is given back.

use super::node::Entry;
use super::{Inner, RamFs};
use crate::fs::vfs::{FileKind, FsError, Id};

impl RamFs {
    /// `link(existing, new)`: the regular file at `existing` gains the name
    /// `new`. A directory cannot be linked ([`FsError::IsDir`]; the VFS
    /// answers `EPERM` before it gets here).
    pub(super) fn link_names(&self, existing: &str, new: &str, caller: Id) -> Result<(), FsError> {
        let mut inner = self.inner.lock();
        let ino = Self::resolve(&inner, existing)?;
        if inner.nodes[&ino].kind != FileKind::File {
            return Err(FsError::IsDir);
        }
        let (parent, name) = Self::resolve_parent(&inner, new)?;
        if Self::child(&inner, parent, &name).is_some() {
            return Err(FsError::Exists);
        }
        Self::node_mut(&mut inner, parent)
            .children
            .try_reserve(1)
            .map_err(|_| FsError::NoSpace)?;
        Self::charge_name(&mut inner, caller.uid)?;
        Self::node_mut(&mut inner, parent).children.push(Entry {
            name,
            ino,
            extra: Some(caller.uid),
        });
        let node = Self::node_mut(&mut inner, ino);
        node.links += 1;
        node.touch_ctime();
        Ok(())
    }

    /// Take the entry `name` out of directory `parent` and drop its share of
    /// the node: a file goes with its last name, a directory (empty: the
    /// callers checked) with its only one, and its `..` with it.
    pub(super) fn drop_name(inner: &mut Inner, parent: u64, name: &str) {
        let dir = Self::node_mut(inner, parent);
        let Some(at) = dir.children.iter().position(|entry| entry.name == name) else {
            return;
        };
        let entry = dir.children.remove(at);
        let ino = entry.ino;
        if inner.nodes[&ino].kind == FileKind::Dir {
            let dir = Self::node_mut(inner, parent);
            dir.links = dir.links.saturating_sub(1);
            let removed = Self::take_node(inner, ino);
            Self::forget_node(inner, &removed);
            return;
        }
        let node = Self::node_mut(inner, ino);
        node.links = node.links.saturating_sub(1);
        if node.links == 0 {
            let removed = Self::take_node(inner, ino);
            Self::forget_node(inner, &removed);
            return;
        }
        node.touch_ctime();
        let payer = match entry.extra {
            Some(uid) => Some(uid),
            // The name the node's charge covered went: another takes over.
            None => Self::promote_name(inner, ino),
        };
        if let Some(uid) = payer {
            Self::release_name(inner, uid);
        }
    }

    /// Make another name of file `ino` the one its node's charge covers, and
    /// return who paid for that name until now. A scan of every directory:
    /// only when a linked file loses the name it was created under.
    fn promote_name(inner: &mut Inner, ino: u64) -> Option<u32> {
        inner
            .nodes
            .values_mut()
            .flat_map(|node| node.children.iter_mut())
            .find(|entry| entry.ino == ino && entry.extra.is_some())
            .and_then(|entry| entry.extra.take())
    }

    /// `rename(from, to)`: a file replaces a file (dropping one name of it), a
    /// directory replaces only an empty directory and cannot move below
    /// itself. Two names of one file are a no-op, as POSIX says.
    pub(super) fn rename_names(&self, from: &str, to: &str) -> Result<(), FsError> {
        let mut inner = self.inner.lock();
        let (from_parent, from_name) = Self::resolve_parent(&inner, from)?;
        let source = Self::child(&inner, from_parent, &from_name).ok_or(FsError::NotFound)?;
        let (to_parent, to_name) = Self::resolve_parent(&inner, to)?;

        // Renaming a node onto itself (any two spellings of one path, or two
        // hard links of one file) is a no-op, not a replace: removing the
        // "existing" name would remove one of the source's.
        let existing = Self::child(&inner, to_parent, &to_name);
        if existing == Some(source) {
            return Ok(());
        }
        let source_kind = inner.nodes[&source].kind;
        // A directory cannot move beneath itself: that would detach the
        // subtree into a cycle unreachable from the root.
        if source_kind == FileKind::Dir && Self::is_within(&inner, source, to_parent)? {
            return Err(FsError::Invalid);
        }
        // The destination may exist: a file replaces a file, a directory may
        // replace only an empty directory (POSIX would allow exactly this set).
        if let Some(existing) = existing {
            match (source_kind, inner.nodes[&existing].kind) {
                (FileKind::File, FileKind::File) => {}
                (FileKind::Dir, FileKind::Dir) => {
                    if !inner.nodes[&existing].children.is_empty() {
                        return Err(FsError::NotEmpty);
                    }
                }
                (FileKind::File, FileKind::Dir) => return Err(FsError::IsDir),
                (FileKind::Dir, FileKind::File) => return Err(FsError::NotDir),
            }
        }
        // The one allocation, before anything changes.
        Self::node_mut(&mut inner, to_parent)
            .children
            .try_reserve(1)
            .map_err(|_| FsError::NoSpace)?;
        if existing.is_some() {
            Self::drop_name(&mut inner, to_parent, &to_name);
        }
        let dir = Self::node_mut(&mut inner, from_parent);
        let Some(at) = dir
            .children
            .iter()
            .position(|entry| entry.name == from_name)
        else {
            return Err(FsError::NotFound); // unreachable: resolved above
        };
        let mut entry = dir.children.remove(at);
        entry.name = to_name;
        if source_kind == FileKind::Dir && from_parent != to_parent {
            // The moved directory's `..` changes parent.
            let old = Self::node_mut(&mut inner, from_parent);
            old.links = old.links.saturating_sub(1);
            Self::node_mut(&mut inner, to_parent).links += 1;
        }
        Self::node_mut(&mut inner, to_parent).children.push(entry);
        Ok(())
    }
}
