//! The registry of open files: which [`Inode`] a file is, by the file rather
//! than by the name it was reached through (issue #871).
//!
//! Where the filesystem's nodes are its files ([`Node::stable`]: ext2, ramfs),
//! a file is its filesystem instance and its node ([`Node::same_file`]): the
//! two names of a hard-linked file, or the two mount points of one ramfs,
//! find one [`Inode`] and so one page cache, and a shared mapping through one
//! name sees the stores made through the other. Elsewhere (the copy-up root,
//! FAT, a user-space filesystem whose daemon numbers its nodes) there are no
//! links, and the path is the file, its node only telling a name's old file
//! from its new one.

use alloc::string::String;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use super::image::{self, Role};
use super::pagecache::PageCache;
use super::{canonical, mapping, Inode, Names, OPEN};
use crate::fs::vfs::{FileKind, FsError, Id, Node};

/// The shared [`Inode`] of the file `node` (or, without a node, the file at
/// `path`), registering a new one if it is not open.
///
/// A name can have been given to another file behind this registry's back
/// (the native VFS deletes and renames without it): an entry still holding
/// `path` as its name that is not this file loses it, so lookups by path
/// (`unlink`, `rename`) find the file that has it now; its descriptors keep
/// their node. A file found open under no current name takes `path`.
pub(super) fn share_inode(
    path: String,
    shown: String,
    node: Option<Node>,
    role: Role,
) -> Result<Arc<Inode>, FsError> {
    let mut open = OPEN.lock();
    let stable = node.as_ref().filter(|node| node.stable());
    let found = open
        .iter()
        .find(|inode| match stable {
            Some(node) => inode.node.as_ref().is_some_and(|held| held.same_file(node)),
            None => inode.is_named(&path) && same_node(inode.node.as_ref(), node.as_ref()),
        })
        .cloned();
    for other in open.iter() {
        let this = found
            .as_ref()
            .is_some_and(|found| Arc::ptr_eq(found, other));
        if !this && other.is_named(&path) {
            other.forget_key();
        }
    }
    if let Some(found) = found {
        image::claim(&found, role)?;
        found.descriptions.fetch_add(1, Ordering::Relaxed);
        if found.path().is_empty() {
            found.set_key(path);
        }
        return Ok(found);
    }
    open.try_reserve(1).map_err(|_| FsError::NoSpace)?;
    let inode = Arc::new(Inode {
        names: Mutex::new(Names {
            key: path,
            current: true,
            shown,
        }),
        orphan: AtomicBool::new(false),
        node,
        descriptions: AtomicUsize::new(1),
        text: AtomicUsize::new(0),
        writers: AtomicUsize::new(0),
        cache: PageCache::new(),
        failure_logged: AtomicBool::new(false),
    });
    image::claim(&inode, role)?;
    open.push(Arc::clone(&inode));
    Ok(inode)
}

/// Whether two optional nodes are the same file (both absent counts).
fn same_node(held: Option<&Node>, node: Option<&Node>) -> bool {
    match (held, node) {
        (Some(held), Some(node)) => held.same_file(node),
        (None, None) => true,
        _ => false,
    }
}

/// Drop one description's claim on `inode`; true when it was the last, in which
/// case the inode leaves the registry.
pub(super) fn release(inode: &Arc<Inode>, role: Role) -> bool {
    let mut open = OPEN.lock();
    image::unclaim(inode, role);
    // Descriptions are counted, not inferred from the `Arc`: a transient
    // holder (`rmdir`'s parked list, a `Displaced`) must not hide the last
    // close from this check.
    if inode.descriptions.fetch_sub(1, Ordering::Relaxed) > 1 {
        return false;
    }
    open.retain(|entry| !Arc::ptr_eq(entry, inode));
    true
}

/// The open inode of the file `path` names now, whichever of its names
/// `path` is.
///
/// The cheap question first: the name's filesystem and inode number, from the
/// mount table's caches, against the open files. Only when some open file has
/// that number is the node opened, and then every open file is compared with
/// it by generation: an entry left by a file deleted behind the registry's
/// back (a native unlink) may share the number with the file that reused it.
/// A file without a stable node is matched by name.
pub(super) fn find(path: &str) -> Option<Arc<Inode>> {
    let path = canonical(path);
    if OPEN.lock().is_empty() {
        return None;
    }
    let (fs, meta) = crate::fs::nodes::abi_identity(&path).ok()?;
    if meta.kind != FileKind::File {
        return None;
    }
    let stable = |inode: &Inode| inode.node.clone().filter(Node::stable);
    let numbered = OPEN.lock().iter().any(|inode| {
        stable(inode).is_some_and(|node| node.fs_key() == fs && node.id().ino == meta.ino)
    });
    if !numbered {
        return OPEN
            .lock()
            .iter()
            .find(|inode| stable(inode).is_none() && inode.is_named(&path))
            .cloned();
    }
    let now = match crate::fs::nodes::abi_open_node(Id::ROOT, &path, 0) {
        Ok(Some(now)) => now,
        _ => return None,
    };
    OPEN.lock()
        .iter()
        .find(|inode| stable(inode).is_some_and(|held| held.same_file(&now)))
        .cloned()
}

/// The contents of the file at `path` changed by name (`truncate(2)`,
/// `O_TRUNC`, a path write): an open file's cached pages past its new end go,
/// and the tail of the last one is zeroed (`mmap` of it sees the new size).
pub(crate) fn content_changed(path: &str, native: bool) {
    let Some(inode) = find(path) else {
        return;
    };
    // Whatever the cache is, a fill parked on the file must not insert what
    // it read before this change.
    inode.cache.invalidate_fills();
    if inode.cache.is_empty() {
        return;
    }
    if native {
        // Bytes changed behind the cache: refresh its clean pages.
        mapping::refresh(&inode);
    } else if let Ok(size) = inode.size() {
        inode.truncated(size);
    }
}
