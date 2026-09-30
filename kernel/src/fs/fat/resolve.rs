//! Path resolution for the FAT volume: walk components from the root, with a
//! small cache of resolved paths. The volume is read-only, so a resolved path
//! stays valid forever and each `read` need not re-walk directories.

use super::dir::{DirLoc, Entry};
use super::Fat16;
use crate::fs::vfs::FsError;
use alloc::string::String;
use alloc::vec::Vec;

/// Resolved paths kept (oldest evicted first).
const CACHE_SLOTS: usize = 64;
/// Inode of the volume root; entry positions are always larger.
pub(super) const ROOT_INO: u64 = 1;

/// A resolved file or directory.
#[derive(Clone)]
pub(super) struct Node {
    pub ino: u64,
    pub cluster: u16,
    pub size: u32,
    pub is_dir: bool,
}

impl Node {
    const ROOT: Node = Node {
        ino: ROOT_INO,
        cluster: 0,
        size: 0,
        is_dir: true,
    };

    fn loc(&self) -> DirLoc {
        if self.ino == ROOT_INO {
            DirLoc::Root
        } else {
            DirLoc::Chain(self.cluster)
        }
    }
}

impl From<&Entry> for Node {
    fn from(entry: &Entry) -> Node {
        Node {
            ino: entry.ino,
            cluster: entry.cluster,
            size: entry.size,
            is_dir: entry.is_dir,
        }
    }
}

/// FIFO cache of resolved paths.
pub(super) struct PathCache(Vec<(String, Node)>);

impl PathCache {
    pub(super) const fn new() -> Self {
        PathCache(Vec::new())
    }
}

impl Fat16 {
    /// Resolve `path` (ASCII case-insensitive) to its node.
    pub(super) fn resolve(&self, path: &str) -> Result<Node, FsError> {
        let parts = || path.split('/').filter(|part| !part.is_empty());
        let mut key = String::new();
        for part in parts() {
            key.push('/');
            key.push_str(&part.to_ascii_uppercase());
        }
        if key.is_empty() {
            return Ok(Node::ROOT);
        }
        if let Some((_, node)) = self.paths.lock().0.iter().find(|(k, _)| *k == key) {
            return Ok(node.clone());
        }
        let mut node = Node::ROOT;
        for part in parts() {
            if !node.is_dir {
                return Err(FsError::NotDir);
            }
            node = self.find_in(&node, part)?;
        }
        let mut cache = self.paths.lock();
        if cache.0.len() >= CACHE_SLOTS {
            cache.0.remove(0);
        }
        cache.0.push((key, node.clone()));
        Ok(node)
    }

    /// Find `name` inside directory `dir`, by long or short name.
    fn find_in(&self, dir: &Node, name: &str) -> Result<Node, FsError> {
        for entry in self.scan(dir.loc()) {
            let entry = entry?;
            if entry.name.eq_ignore_ascii_case(name) || entry.short.eq_ignore_ascii_case(name) {
                return Ok(Node::from(&entry));
            }
        }
        Err(FsError::NotFound)
    }

    /// The entries of directory `dir`, names as stored.
    pub(super) fn read_dir(&self, dir: &Node) -> Result<Vec<Entry>, FsError> {
        if !dir.is_dir {
            return Err(FsError::NotDir);
        }
        self.scan(dir.loc()).collect()
    }
}
