//! A regular file's contents, by inode number: what the path calls and the
//! node calls of [`Filesystem`](crate::fs::vfs::Filesystem) share, and the
//! node an open file holds (`vfs/node.rs`).
//!
//! Inode numbers are never reused (`next_ino` only grows), so a [`NodeId`]
//! keeps naming one file whatever happens to its names, and a node whose
//! file is gone answers [`FsError::NotFound`]. The generation is always 0.

use super::{Inner, RamFs};
use crate::fs::vfs::{FileKind, FsError, NodeId};

impl RamFs {
    /// The node of the regular file at `path`.
    pub(super) fn open_file_node(&self, path: &str) -> Result<NodeId, FsError> {
        let inner = self.inner.lock();
        let ino = Self::resolve(&inner, path)?;
        if inner.nodes[&ino].kind != FileKind::File {
            return Err(FsError::IsDir);
        }
        Ok(NodeId { ino, generation: 0 })
    }

    /// The inode `node` names, if it is still a regular file of this tree.
    pub(super) fn file_ino(inner: &Inner, node: NodeId) -> Result<u64, FsError> {
        match inner.nodes.get(&node.ino) {
            Some(found) if node.generation == 0 && found.kind == FileKind::File => Ok(node.ino),
            _ => Err(FsError::NotFound),
        }
    }

    pub(super) fn read_ino(
        inner: &Inner,
        ino: u64,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, FsError> {
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

    pub(super) fn write_ino(
        inner: &mut Inner,
        ino: u64,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, FsError> {
        if inner.nodes[&ino].kind != FileKind::File {
            return Err(FsError::IsDir);
        }
        let end = offset
            .checked_add(data.len() as u64)
            .and_then(|end| usize::try_from(end).ok())
            .ok_or(FsError::NoSpace)?;
        if end > inner.nodes[&ino].data.len() {
            Self::resize_file(inner, ino, end)?;
        }
        let node = Self::node_mut(inner, ino);
        node.data[offset as usize..end].copy_from_slice(data);
        node.touch();
        Ok(data.len())
    }

    pub(super) fn truncate_ino(inner: &mut Inner, ino: u64, size: u64) -> Result<(), FsError> {
        if inner.nodes[&ino].kind != FileKind::File {
            return Err(FsError::IsDir);
        }
        let size = usize::try_from(size).map_err(|_| FsError::NoSpace)?;
        Self::resize_file(inner, ino, size)?;
        Self::node_mut(inner, ino).touch();
        Ok(())
    }
}
