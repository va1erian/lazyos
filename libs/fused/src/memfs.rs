//! An in-memory [`FuseFs`]: the `memfuse` daemon's tree, which proves the
//! mechanism with no network at all (docs/smb-plan.md stage F1), and the
//! filesystem the kernel suite serves through the real kernel path.
//!
//! Inode numbers are never reused within one tree, so a node handle of a
//! deleted file can never reach a later one; the generation is the creation
//! sequence number all the same, so a handle must match both. File bytes
//! are bounded by the capacity given at construction (`ENOSPC` past it), and
//! so is the number of nodes.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::daemon::{Errno, FuseFs, Target};
use crate::payload::{set, DirEnt, SetAttrRecord, StatFsRecord};
use crate::wire::{errno, Attr, MAX_NAME, S_IFDIR, S_IFREG};

/// The `statfs` magic `memfuse` reports ("memf").
pub const MAGIC: u64 = 0x6d65_6d66;
const ROOT: u64 = 1;
const BLOCK: u64 = 4096;

enum Body {
    File(Vec<u8>),
    Dir(BTreeMap<String, u64>),
}

struct Node {
    generation: u64,
    mode: u16,
    uid: u32,
    gid: u32,
    times: [i64; 3],
    body: Body,
}

/// The tree.
pub struct MemFs {
    nodes: BTreeMap<u64, Node>,
    next_ino: u64,
    used: usize,
    capacity: usize,
    max_nodes: usize,
    now: fn() -> i64,
}

impl MemFs {
    /// An empty tree whose root is `0755` owned by `uid`/`gid`, holding at
    /// most `capacity` bytes of file data and `max_nodes` nodes, stamping
    /// times from `now`.
    pub fn new(capacity: usize, max_nodes: usize, uid: u32, gid: u32, now: fn() -> i64) -> MemFs {
        let mut nodes = BTreeMap::new();
        let time = now();
        nodes.insert(
            ROOT,
            Node {
                generation: 0,
                mode: 0o755,
                uid,
                gid,
                times: [time; 3],
                body: Body::Dir(BTreeMap::new()),
            },
        );
        MemFs {
            nodes,
            next_ino: ROOT + 1,
            used: 0,
            capacity,
            max_nodes: max_nodes.max(1),
            now,
        }
    }

    /// Bytes of file data held.
    pub fn used(&self) -> usize {
        self.used
    }

    /// Nodes in the tree, the root included.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    fn attr(&self, ino: u64) -> Result<Attr, Errno> {
        let node = self.nodes.get(&ino).ok_or(errno::ENOENT)?;
        let (kind, size) = match &node.body {
            Body::File(data) => (S_IFREG, data.len() as u64),
            Body::Dir(entries) => (S_IFDIR, entries.len() as u64),
        };
        Ok(Attr {
            ino,
            generation: node.generation,
            mode: kind | u64::from(node.mode),
            uid: u64::from(node.uid),
            gid: u64::from(node.gid),
            size,
            atime: node.times[0],
            mtime: node.times[1],
            ctime: node.times[2],
        })
    }

    /// The inode `path` names.
    fn walk(&self, path: &str) -> Result<u64, Errno> {
        let mut ino = ROOT;
        if path.is_empty() {
            return Ok(ino);
        }
        for name in path.split('/') {
            match &self.nodes.get(&ino).ok_or(errno::ENOENT)?.body {
                Body::Dir(entries) => ino = *entries.get(name).ok_or(errno::ENOENT)?,
                Body::File(_) => return Err(errno::ENOTDIR),
            }
        }
        Ok(ino)
    }

    fn resolve(&self, target: Target) -> Result<u64, Errno> {
        match target {
            Target::Path(path) => self.walk(path),
            Target::Node { ino, generation } => match self.nodes.get(&ino) {
                Some(node) if node.generation == generation => Ok(ino),
                _ => Err(errno::ESTALE),
            },
        }
    }

    /// The parent directory's inode and the last name of `path`.
    fn parent<'p>(&self, path: &'p str) -> Result<(u64, &'p str), Errno> {
        let (dir, name) = match path.rsplit_once('/') {
            Some((dir, name)) => (dir, name),
            None if path.is_empty() => return Err(errno::EEXIST),
            None => ("", path),
        };
        if name.len() > MAX_NAME {
            return Err(errno::ENAMETOOLONG);
        }
        let ino = self.walk(dir)?;
        match self.nodes[&ino].body {
            Body::Dir(_) => Ok((ino, name)),
            Body::File(_) => Err(errno::ENOTDIR),
        }
    }

    fn entries_mut(&mut self, dir: u64) -> &mut BTreeMap<String, u64> {
        match &mut self.nodes.get_mut(&dir).expect("parent is a node").body {
            Body::Dir(entries) => entries,
            Body::File(_) => unreachable!("parent is a directory"),
        }
    }

    fn file_mut(&mut self, ino: u64) -> Result<&mut Vec<u8>, Errno> {
        match &mut self.nodes.get_mut(&ino).ok_or(errno::ENOENT)?.body {
            Body::File(data) => Ok(data),
            Body::Dir(_) => Err(errno::EISDIR),
        }
    }

    fn touch(&mut self, ino: u64) {
        let time = (self.now)();
        if let Some(node) = self.nodes.get_mut(&ino) {
            node.times[1] = time;
            node.times[2] = time;
        }
    }

    /// Resize file `ino` to `len`, within the capacity.
    fn resize(&mut self, ino: u64, len: u64) -> Result<(), Errno> {
        let len = usize::try_from(len).map_err(|_| errno::ENOSPC)?;
        let old = self.file_mut(ino)?.len();
        if len > old && len - old > self.capacity - self.used {
            return Err(errno::ENOSPC);
        }
        let data = self.file_mut(ino)?;
        if data.try_reserve(len.saturating_sub(old)).is_err() {
            return Err(errno::ENOSPC);
        }
        data.resize(len, 0);
        if len >= old {
            self.used += len - old;
        } else {
            self.used -= old - len;
        }
        Ok(())
    }

    fn insert(&mut self, path: &str, mode: u16, uid: u32, gid: u32, body: Body) -> Result<Attr, Errno> {
        let (dir, name) = self.parent(path)?;
        if self.entries_mut(dir).contains_key(name) {
            return Err(errno::EEXIST);
        }
        if self.nodes.len() >= self.max_nodes {
            return Err(errno::ENOSPC);
        }
        let ino = self.next_ino;
        self.next_ino += 1;
        let time = (self.now)();
        self.nodes.insert(
            ino,
            Node {
                generation: ino,
                mode: mode & 0o7777,
                uid,
                gid,
                times: [time; 3],
                body,
            },
        );
        self.entries_mut(dir).insert(String::from(name), ino);
        self.touch(dir);
        self.attr(ino)
    }

    /// Drop node `ino` (already unlinked from its directory).
    fn forget(&mut self, ino: u64) {
        if let Some(Node {
            body: Body::File(data),
            ..
        }) = self.nodes.remove(&ino)
        {
            self.used -= data.len();
        }
    }

    fn is_empty_dir(&self, ino: u64) -> Option<bool> {
        match &self.nodes.get(&ino)?.body {
            Body::Dir(entries) => Some(entries.is_empty()),
            Body::File(_) => None,
        }
    }
}

impl FuseFs for MemFs {
    fn lookup(&mut self, target: Target) -> Result<Attr, Errno> {
        let ino = self.resolve(target)?;
        self.attr(ino)
    }

    fn read(&mut self, target: Target, offset: u64, out: &mut [u8]) -> Result<usize, Errno> {
        let ino = self.resolve(target)?;
        let data = self.file_mut(ino)?;
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(data.len());
        let count = out.len().min(data.len() - start);
        out[..count].copy_from_slice(&data[start..start + count]);
        Ok(count)
    }

    fn write(&mut self, target: Target, offset: u64, data: &[u8]) -> Result<usize, Errno> {
        let ino = self.resolve(target)?;
        let end = offset.checked_add(data.len() as u64).ok_or(errno::EINVAL)?;
        let len = self.file_mut(ino)?.len() as u64;
        if end > len {
            self.resize(ino, end)?;
        }
        let start = offset as usize;
        self.file_mut(ino)?[start..start + data.len()].copy_from_slice(data);
        self.touch(ino);
        Ok(data.len())
    }

    fn truncate(&mut self, target: Target, size: u64) -> Result<(), Errno> {
        let ino = self.resolve(target)?;
        self.resize(ino, size)?;
        self.touch(ino);
        Ok(())
    }

    fn setattr(&mut self, path: &str, change: &SetAttrRecord) -> Result<Attr, Errno> {
        let ino = self.walk(path)?;
        let node = self.nodes.get_mut(&ino).ok_or(errno::ENOENT)?;
        let stamps = [
            (set::ATIME, change.atime),
            (set::MTIME, change.mtime),
            (set::CTIME, change.ctime),
        ];
        for (slot, (bit, value)) in node.times.iter_mut().zip(stamps) {
            if change.mask & bit != 0 {
                *slot = value;
            }
        }
        if change.mask & set::MODE != 0 {
            node.mode = (change.mode & 0o7777) as u16;
        }
        if change.mask & set::UID != 0 {
            node.uid = change.uid as u32;
        }
        if change.mask & set::GID != 0 {
            node.gid = change.gid as u32;
        }
        self.attr(ino)
    }

    fn create(&mut self, path: &str, mode: u16, uid: u32, gid: u32) -> Result<Attr, Errno> {
        self.insert(path, mode, uid, gid, Body::File(Vec::new()))
    }

    fn mkdir(&mut self, path: &str, mode: u16, uid: u32, gid: u32) -> Result<Attr, Errno> {
        self.insert(path, mode, uid, gid, Body::Dir(BTreeMap::new()))
    }

    fn unlink(&mut self, path: &str) -> Result<(), Errno> {
        let (dir, name) = self.parent(path)?;
        let ino = *self.entries_mut(dir).get(name).ok_or(errno::ENOENT)?;
        if self.is_empty_dir(ino).is_some() {
            return Err(errno::EISDIR);
        }
        self.entries_mut(dir).remove(name);
        self.forget(ino);
        self.touch(dir);
        Ok(())
    }

    fn rmdir(&mut self, path: &str) -> Result<(), Errno> {
        if path.is_empty() {
            return Err(errno::EPERM);
        }
        let (dir, name) = self.parent(path)?;
        let ino = *self.entries_mut(dir).get(name).ok_or(errno::ENOENT)?;
        match self.is_empty_dir(ino) {
            None => return Err(errno::ENOTDIR),
            Some(false) => return Err(errno::ENOTEMPTY),
            Some(true) => {}
        }
        self.entries_mut(dir).remove(name);
        self.forget(ino);
        self.touch(dir);
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), Errno> {
        if from.is_empty() || to.is_empty() {
            return Err(errno::EPERM);
        }
        if to.len() > from.len() && to.starts_with(from) && to.as_bytes()[from.len()] == b'/' {
            return Err(errno::EINVAL);
        }
        let (from_dir, from_name) = self.parent(from)?;
        let (to_dir, to_name) = self.parent(to)?;
        let ino = *self.entries_mut(from_dir).get(from_name).ok_or(errno::ENOENT)?;
        if from == to {
            return Ok(());
        }
        let moving_dir = self.is_empty_dir(ino).is_some();
        if let Some(&old) = self.entries_mut(to_dir).get(to_name) {
            match (moving_dir, self.is_empty_dir(old)) {
                (false, Some(_)) => return Err(errno::EISDIR),
                (true, None) => return Err(errno::ENOTDIR),
                (true, Some(false)) => return Err(errno::ENOTEMPTY),
                _ => {}
            }
            self.forget(old);
        }
        self.entries_mut(from_dir).remove(from_name);
        self.entries_mut(to_dir).insert(String::from(to_name), ino);
        self.touch(from_dir);
        self.touch(to_dir);
        Ok(())
    }

    fn readdir(&mut self, path: &str) -> Result<Vec<DirEnt>, Errno> {
        let ino = self.walk(path)?;
        let Body::Dir(entries) = &self.nodes[&ino].body else {
            return Err(errno::ENOTDIR);
        };
        Ok(entries
            .iter()
            .map(|(name, &child)| DirEnt {
                ino: child,
                dir: matches!(self.nodes[&child].body, Body::Dir(_)),
                name: name.clone(),
            })
            .collect())
    }

    fn statfs(&mut self) -> Result<StatFsRecord, Errno> {
        Ok(StatFsRecord {
            magic: MAGIC,
            block_size: BLOCK,
            blocks: self.capacity as u64 / BLOCK,
            blocks_free: (self.capacity - self.used) as u64 / BLOCK,
            files: self.max_nodes as u64,
            files_free: (self.max_nodes - self.nodes.len()) as u64,
            name_max: MAX_NAME as u64,
        })
    }
}
