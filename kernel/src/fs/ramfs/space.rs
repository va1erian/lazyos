//! What a ramfs holds, per filesystem and per owner (issue #265).
//!
//! The filesystem-wide caps (`max_bytes`, `max_nodes`) keep `/tmp` from
//! exhausting the kernel heap; the per-uid caps keep one user from filling it
//! for everyone. [`Filesystem`](crate::fs::vfs::Filesystem) writes carry no
//! caller, so a node's bytes are charged to its *owner*, as disk quotas do:
//! whoever writes into a file, its owner pays, and `chown` moves the charge.
//! Root (uid 0) is exempt from the per-uid caps (it owns the tree's
//! skeleton and is still bound by the filesystem caps).
//!
//! Every allocation here is fallible, like the rest of the ramfs: running out
//! of heap is `ENOSPC`, never an abort.

use alloc::string::String;
use alloc::vec::Vec;

use super::node::Node;
use super::{Inner, RamFs};
use crate::fs::vfs::{FileKind, FsError, Id, Meta};

/// What one owner holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub bytes: usize,
    pub nodes: usize,
}

/// Per-owner usage and the per-owner caps.
pub(super) struct Owners {
    usage: Vec<(u32, Usage)>,
    pub(super) max_bytes: usize,
    pub(super) max_nodes: usize,
}

impl Owners {
    pub(super) const fn new(max_bytes: usize, max_nodes: usize) -> Owners {
        Owners {
            usage: Vec::new(),
            max_bytes,
            max_nodes,
        }
    }

    pub(super) fn of(&self, uid: u32) -> Usage {
        self.usage
            .iter()
            .find(|(owner, _)| *owner == uid)
            .map_or(Usage::default(), |(_, usage)| *usage)
    }

    /// Whether `uid` may take `bytes` more bytes and `nodes` more nodes, with
    /// room reserved to record them, so the matching [`Owners::add`] cannot
    /// fail.
    fn admit(&mut self, uid: u32, bytes: usize, nodes: usize) -> Result<(), FsError> {
        let held = self.of(uid);
        if uid != 0
            && (held.bytes.saturating_add(bytes) > self.max_bytes
                || held.nodes.saturating_add(nodes) > self.max_nodes)
        {
            return Err(FsError::NoSpace);
        }
        self.reserve(uid)
    }

    /// Make sure recording `uid` needs no allocation.
    fn reserve(&mut self, uid: u32) -> Result<(), FsError> {
        if self.usage.iter().any(|(owner, _)| *owner == uid) {
            return Ok(());
        }
        self.usage.try_reserve(1).map_err(|_| FsError::NoSpace)
    }

    fn add(&mut self, uid: u32, bytes: usize, nodes: usize) {
        match self.usage.iter_mut().find(|(owner, _)| *owner == uid) {
            Some((_, usage)) => {
                usage.bytes += bytes;
                usage.nodes += nodes;
            }
            None => self.usage.push((uid, Usage { bytes, nodes })),
        }
    }

    fn sub(&mut self, uid: u32, bytes: usize, nodes: usize) {
        if let Some((_, usage)) = self.usage.iter_mut().find(|(owner, _)| *owner == uid) {
            usage.bytes = usage.bytes.saturating_sub(bytes);
            usage.nodes = usage.nodes.saturating_sub(nodes);
        }
        self.usage.retain(|(_, usage)| *usage != Usage::default());
    }
}

impl RamFs {
    /// What `uid` owns in this filesystem.
    pub fn usage_of(&self, uid: u32) -> Usage {
        self.inner.lock().owners.of(uid)
    }

    /// Resize file `ino` to `new_len` bytes, charging growth against the
    /// filesystem's byte cap and the owner's, and reserving with
    /// `try_reserve_exact`, so running out of heap is `ENOSPC` rather than an
    /// allocation-failure abort. The accounting only moves once the
    /// allocation succeeded.
    pub(super) fn resize_file(inner: &mut Inner, ino: u64, new_len: usize) -> Result<(), FsError> {
        let (old_len, owner) = {
            let node = &inner.nodes[&ino];
            (node.data.len(), node.owner())
        };
        let total = inner.bytes - old_len;
        match total.checked_add(new_len) {
            Some(next) if next <= inner.max_bytes => {}
            _ => return Err(FsError::NoSpace),
        }
        if new_len > old_len {
            inner.owners.admit(owner, new_len - old_len, 0)?;
        }
        let node = Self::node_mut(inner, ino);
        if new_len > old_len {
            node.data
                .try_reserve_exact(new_len - old_len)
                .map_err(|_| FsError::NoSpace)?;
        }
        node.data.resize(new_len, 0); // zero-fills a sparse gap
        inner.bytes = total + new_len;
        if new_len > old_len {
            inner.owners.add(owner, new_len - old_len, 0);
        } else {
            inner.owners.sub(owner, old_len - new_len, 0);
        }
        Ok(())
    }

    /// Whether one more node owned by `owner` fits under both caps.
    pub(super) fn check_node_room(inner: &mut Inner, owner: u32) -> Result<(), FsError> {
        if inner.live >= inner.max_nodes {
            return Err(FsError::NoSpace);
        }
        inner.owners.admit(owner, 0, 1)
    }

    /// Create a node and link it into its parent; the caller ran
    /// [`RamFs::check_node_room`] for `owner`.
    pub(super) fn insert(
        inner: &mut Inner,
        parent: u64,
        name: String,
        kind: FileKind,
        mode: u16,
        owner: Id,
    ) -> Meta {
        let ino = inner.next_ino;
        inner.next_ino += 1;
        inner.nodes.insert(ino, Node::new(name, kind, mode, owner));
        Self::node_mut(inner, parent).children.push(ino);
        inner.live += 1;
        inner.owners.add(owner.uid, 0, 1);
        inner.nodes[&ino].meta(ino)
    }

    /// Account for a node that left the tree.
    pub(super) fn forget_node(inner: &mut Inner, removed: &Node) {
        inner.bytes -= removed.data.len();
        inner.live -= 1;
        inner.owners.sub(removed.owner(), removed.data.len(), 1);
    }

    /// Move node `ino`'s charge to `new_owner` ahead of a `chown`. The
    /// per-uid cap is not applied (changing an owner is privileged, and a
    /// `setattr` must not half-apply); only the bookkeeping room is reserved,
    /// before anything changes.
    pub(super) fn move_charge(inner: &mut Inner, ino: u64, new_owner: u32) -> Result<(), FsError> {
        let (old_owner, bytes) = {
            let node = &inner.nodes[&ino];
            (node.owner(), node.data.len())
        };
        if old_owner == new_owner {
            return Ok(());
        }
        inner.owners.reserve(new_owner)?;
        inner.owners.sub(old_owner, bytes, 1);
        inner.owners.add(new_owner, bytes, 1);
        Ok(())
    }
}
