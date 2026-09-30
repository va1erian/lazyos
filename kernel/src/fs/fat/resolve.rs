//! Path resolution for the FAT12/16 reader (issue #414).
//!
//! Paths arrive relative to the mount, e.g. `usr/bin/tool`. Each component is
//! looked up in the directory before it; matching is ASCII case-insensitive,
//! so `/busybox` finds `BUSYBOX`. The volume is read-only, so a resolved path
//! stays valid for the life of the mount and is kept in a small bounded cache:
//! the VFS resolves every ancestor for its permission checks and then the leaf
//! again on each `read`, and without the cache each of those would re-walk the
//! directories from the root.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;

use super::dir::{Dir, Entry};
use super::Fat16;
use crate::fs::vfs::FsError;

/// Cached paths kept; the oldest is dropped first.
const CACHE_CAP: usize = 128;
/// Paths whose canonical spelling is longer than this are resolved but not
/// cached, which bounds the cache's memory.
const MAX_CACHED_PATH: usize = 256;
/// Deeper paths are refused: no real image nests this far.
const MAX_DEPTH: usize = 64;

/// The root's inode; every other node's is even (see `dir.rs`).
pub(super) const ROOT_INO: u64 = 1;

/// A resolved directory or file.
#[derive(Clone, Copy)]
pub(super) struct Node {
    pub ino: u64,
    pub is_dir: bool,
    /// First cluster (`0` for the root and for empty files).
    pub cluster: u16,
    /// File size in bytes; a directory reports 0 (its size field is unused).
    pub size: u32,
}

impl Node {
    /// Where a directory node's entries live.
    fn as_dir(&self) -> Dir {
        if self.ino == ROOT_INO {
            Dir::Root
        } else {
            Dir::Chain(self.cluster)
        }
    }
}

impl From<Entry> for Node {
    fn from(entry: Entry) -> Node {
        Node {
            ino: entry.ino,
            is_dir: entry.is_dir,
            cluster: entry.cluster,
            size: if entry.is_dir { 0 } else { entry.size },
        }
    }
}

/// Resolved paths, keyed by their ASCII-lowercased canonical spelling.
#[derive(Default)]
pub(super) struct PathCache {
    nodes: BTreeMap<String, Node>,
    order: VecDeque<String>,
}

impl PathCache {
    fn get(&self, key: &str) -> Option<Node> {
        self.nodes.get(key).copied()
    }

    fn insert(&mut self, key: &str, node: Node) {
        if key.len() > MAX_CACHED_PATH || self.nodes.contains_key(key) {
            return;
        }
        if self.order.len() >= CACHE_CAP {
            if let Some(oldest) = self.order.pop_front() {
                self.nodes.remove(&oldest);
            }
        }
        self.nodes.insert(String::from(key), node);
        self.order.push_back(String::from(key));
    }
}

impl Fat16 {
    /// The root directory as a node.
    pub(super) fn root_node(&self) -> Node {
        Node {
            ino: ROOT_INO,
            is_dir: true,
            cluster: 0,
            size: u32::from(self.root_entries) * 32,
        }
    }

    /// Resolve `path` (relative to the mount) to a node.
    ///
    /// [`FsError::NotFound`] when a component is missing (`.` and `..` never
    /// are found), [`FsError::NotDir`] when a file is used as a directory,
    /// [`FsError::Invalid`] when a directory on the way is corrupt.
    pub(super) fn resolve(&self, path: &str) -> Result<Node, FsError> {
        let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        if parts.len() > MAX_DEPTH {
            return Err(FsError::NotFound);
        }
        let mut key = String::new();
        let mut node = self.root_node();
        for part in parts {
            if !node.is_dir {
                return Err(FsError::NotDir);
            }
            if !key.is_empty() {
                key.push('/');
            }
            key.extend(part.chars().map(|ch| ch.to_ascii_lowercase()));
            let cached = self.paths.lock().get(&key);
            node = match cached {
                Some(hit) => hit,
                None => {
                    // The cache lock is not held across the disk reads.
                    let entry = self
                        .find_in(node.as_dir(), part)?
                        .ok_or(FsError::NotFound)?;
                    let found = Node::from(entry);
                    self.paths.lock().insert(&key, found);
                    found
                }
            };
        }
        Ok(node)
    }

    /// The entries of the directory at `node`, as stored.
    pub(super) fn list_dir(&self, node: &Node) -> Result<Vec<Entry>, FsError> {
        self.entries(node.as_dir())?.collect()
    }
}

#[cfg(lazyos_tests)]
impl Fat16 {
    /// Paths currently cached; the soak test checks it stays within the cap.
    pub fn cached_paths(&self) -> usize {
        self.paths.lock().nodes.len()
    }
}
