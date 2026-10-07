//! Opening files as VFS nodes from either mount table (`vfs/node.rs`): the
//! descriptor layer (`openfile`) and the program loader (`process::image`)
//! resolve a file once and then read it by node.

use super::vfs::{FsError, Id, Meta, Node};

/// Open the regular file at `path` in the Linux ABI table as a node, after
/// checking `mask` for `id`. `Ok(None)` when its filesystem has no nodes.
pub fn abi_open_node(id: Id, path: &str, mask: u8) -> Result<Option<Node>, FsError> {
    super::abi_with(|vfs| vfs.open_node(id, path, mask)).unwrap_or(Err(FsError::NotFound))
}

/// [`abi_open_node`] in the native table.
pub fn vfs_open_node(id: Id, path: &str, mask: u8) -> Result<Option<Node>, FsError> {
    super::with(|vfs| vfs.open_node(id, path, mask)).unwrap_or(Err(FsError::NotFound))
}

/// After a write through a node: `meta` is the current metadata of `path`
/// in the Linux ABI table's caches.
pub fn abi_refresh(path: &str, meta: Meta) {
    super::abi_with(|vfs| vfs.refresh(path, meta));
    super::coherence::abi_changed(path, super::coherence::Change::Content);
}
