//! Copy-up overlay root for the Linux ABI (issue #136).
//!
//! The shipped boot volume is read-only FAT. The Linux ABI needs a writable
//! namespace without teaching the FAT driver to write, so [`crate::fs::init`]
//! mounts an [`Overlay`] at `/` in the ABI's own mount table (see
//! [`crate::fs::abi_stat`] and friends):
//!
//! - **lower**: the boot filesystem (FAT today), never written through the
//!   overlay;
//! - **upper**: a private in-memory [`RamFs`], discarded on reboot;
//! - **whiteouts**: a set of lower paths hidden by `unlink`/`rmdir`/`rename`.
//!
//! Reads fall through to the lower layer. The first mutation copies the node
//! up into the upper layer (recursively for directories), and later writes go
//! to the upper copy, so the FAT image is never modified. `readdir` unions the
//! two layers: upper entries win, whiteouts hide lower names, and a directory
//! present in both merges. `/tmp` is a separate ramfs mount in the ABI table
//! and is not overlaid.
//!
//! # Layout and lifetime
//!
//! The upper layer lives only in kernel heap for the life of the boot; there
//! is no persistence path back to the lower volume. Native tasks keep using
//! the raw mounts, so a Linux write is invisible to the native VFS until the
//! file is also created there. Upper-layer inodes are reported with the top
//! bit set ([`UPPER_INO`]) because the two layers number from their own roots
//! and an unmarked number could collide in the VFS caches.
//!
//! # Resource cap
//!
//! The upper layer is bounded so a runaway writer cannot drain the kernel
//! heap: [`MAX_UPPER_BYTES`] of file data and [`MAX_UPPER_NODES`] of upper
//! nodes + whiteouts. Exceeding either answers [`FsError::NoSpace`] (ENOSPC to
//! the ABI). [`Overlay::usage`] exposes the counters, and
//! [`Overlay::with_limits`] lets tests drive the cap with tiny values.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use super::ramfs::RamFs;
use super::vfs::{DirEntry, FileKind, Filesystem, FsError, Id, Meta, StatFs};

/// Upper-layer file-data cap (2 MiB of the 16 MiB kernel heap).
pub const MAX_UPPER_BYTES: usize = 2 * 1024 * 1024;
/// Upper-layer node cap: live ramfs nodes plus whiteout entries.
pub const MAX_UPPER_NODES: usize = 1024;

/// Upper-layer nodes report inodes with this bit set, so an upper inode can
/// never collide with a lower one in the VFS dentry/inode caches (each layer
/// numbers from its own root) or with a sibling path that reused the number.
const UPPER_INO: u64 = 1 << 63;

/// The Linux `f_type` for overlayfs (`OVERLAYFS_SUPER_MAGIC`).
const OVERLAYFS_MAGIC: u32 = 0x794c_7630;

/// A copy-up overlay over a read-only `lower` filesystem; see the module docs.
pub struct Overlay {
    lower: Arc<dyn Filesystem>,
    upper: RamFs,
    /// Lower paths hidden by an unlink/rmdir/rename. Keys are canonical
    /// relative paths (no leading slash, no empty components).
    whiteouts: Mutex<BTreeSet<String>>,
    max_bytes: usize,
    max_nodes: usize,
}

impl Overlay {
    /// An overlay over `lower` with the default caps.
    pub fn new(lower: Arc<dyn Filesystem>) -> Overlay {
        Overlay::with_limits(lower, MAX_UPPER_BYTES, MAX_UPPER_NODES)
    }

    /// An overlay with explicit caps; tests use tiny values to drive ENOSPC.
    pub fn with_limits(lower: Arc<dyn Filesystem>, max_bytes: usize, max_nodes: usize) -> Overlay {
        Overlay {
            lower,
            // The upper layer enforces the byte/node cap itself, atomically under
            // its own lock, so concurrent writers cannot race past the check.
            upper: RamFs::with_limits(max_bytes, max_nodes),
            whiteouts: Mutex::new(BTreeSet::new()),
            max_bytes,
            max_nodes,
        }
    }

    /// `(file data bytes, upper nodes + whiteouts)`; the cap inputs.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // used by tests/diagnostics
    pub fn usage(&self) -> (usize, usize) {
        let (bytes, nodes) = self.upper.usage();
        (bytes, nodes + self.whiteouts.lock().len())
    }

    /// Whether a lower entry at `path` is hidden by a whiteout.
    fn hidden(&self, path: &str) -> bool {
        self.whiteouts.lock().contains(path)
    }

    /// Hide the lower entry at `path` (idempotent); bounded by `max_nodes`.
    fn whiteout(&self, path: &str) -> Result<(), FsError> {
        if path.is_empty() {
            return Ok(());
        }
        let mut whiteouts = self.whiteouts.lock();
        if whiteouts.contains(path) {
            return Ok(());
        }
        if self.upper.usage().1 + whiteouts.len() + 1 > self.max_nodes {
            return Err(FsError::NoSpace);
        }
        whiteouts.insert(String::from(path));
        Ok(())
    }

    /// Drop a whiteout because an upper node now shadows the lower entry.
    fn clear_whiteout(&self, path: &str) {
        self.whiteouts.lock().remove(path);
    }

    /// Whether the union sees `path`.
    fn visible(&self, path: &str) -> bool {
        if self.upper.lookup(path).is_ok() {
            return true;
        }
        !self.hidden(path) && self.lower.lookup(path).is_ok()
    }

    /// Read a whole lower file; `size` comes from its metadata.
    fn read_lower_all(&self, path: &str, size: u64) -> Result<Vec<u8>, FsError> {
        let mut data = alloc::vec![0u8; size as usize];
        let mut filled = 0usize;
        while filled < data.len() {
            let read = self.lower.read(path, filled as u64, &mut data[filled..])?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        data.truncate(filled);
        Ok(data)
    }

    /// Ensure `path` exists in the upper layer, recursively copying lower
    /// directories as needed. An existing upper node is left alone.
    fn copy_up(&self, path: &str) -> Result<(), FsError> {
        if path.is_empty() || self.upper.lookup(path).is_ok() {
            return Ok(());
        }
        if let Some((parent, _)) = path.rsplit_once('/') {
            self.copy_up(parent)?;
            // Copying the parent recurses into its children, so `path` may
            // already have arrived in the upper layer.
            if self.upper.lookup(path).is_ok() {
                return Ok(());
            }
        }
        let meta = self.lower.lookup(path)?;
        match meta.kind {
            FileKind::File => {
                let data = self.read_lower_all(path, meta.size)?;
                if self.upper.usage().0 + data.len() > self.max_bytes
                    || self.upper.usage().1 + 1 > self.max_nodes
                {
                    return Err(FsError::NoSpace);
                }
                self.upper
                    .create(path, meta.mode & 0o7777, id_of(meta))
                    .and_then(|_| self.upper.write(path, 0, &data))?;
                Ok(())
            }
            FileKind::Dir => {
                if self.upper.usage().1 + 1 > self.max_nodes {
                    return Err(FsError::NoSpace);
                }
                self.upper.mkdir(path, meta.mode & 0o7777, id_of(meta))?;
                for entry in self.lower.readdir(path)? {
                    let child = join(path, &entry.name);
                    self.copy_up(&child)?;
                }
                Ok(())
            }
        }
    }
}

/// The owner to stamp on a copied-up node: the lower node's owner.
fn id_of(meta: Meta) -> Id {
    Id {
        uid: meta.uid,
        gid: meta.gid,
    }
}

/// Renumber an upper-layer node's inode into the overlay's private namespace.
fn upper_meta(meta: Meta) -> Meta {
    Meta {
        ino: UPPER_INO | meta.ino,
        ..meta
    }
}

/// Canonicalize a trait path: strip empty/`.` components and leading or
/// trailing separators, matching the VFS's relative-path form.
fn canonical(path: &str) -> String {
    path.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// Join a directory key and a child name.
fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        String::from(name)
    } else {
        format!("{parent}/{name}")
    }
}

impl Filesystem for Overlay {
    fn name(&self) -> &'static str {
        "overlay (abi rw)"
    }

    /// New data lands in the upper layer, so its caps are what can still be
    /// written; only the magic says this is an overlay.
    fn statfs(&self) -> Result<StatFs, FsError> {
        Ok(StatFs {
            magic: OVERLAYFS_MAGIC,
            ..self.upper.statfs()?
        })
    }

    fn lookup(&self, path: &str) -> Result<Meta, FsError> {
        let path = canonical(path);
        if let Ok(meta) = self.upper.lookup(&path) {
            return Ok(upper_meta(meta));
        }
        if self.hidden(&path) {
            return Err(FsError::NotFound);
        }
        self.lower.lookup(&path)
    }

    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let path = canonical(path);
        if let Ok(meta) = self.upper.lookup(&path) {
            if meta.kind != FileKind::File {
                return Err(FsError::IsDir);
            }
            return self.upper.read(&path, offset, buf);
        }
        if self.hidden(&path) {
            return Err(FsError::NotFound);
        }
        self.lower.read(&path, offset, buf)
    }

    fn write(&self, path: &str, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        let path = canonical(path);
        self.copy_up(&path)?;
        let current = self.upper.lookup(&path)?.size;
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(FsError::NoSpace)?;
        let extra = end.saturating_sub(current);
        if extra > self.max_bytes.saturating_sub(self.upper.usage().0) as u64 {
            return Err(FsError::NoSpace);
        }
        self.upper.write(&path, offset, data)
    }

    fn truncate(&self, path: &str, size: u64) -> Result<(), FsError> {
        let path = canonical(path);
        self.copy_up(&path)?;
        let current = self.upper.lookup(&path)?.size;
        if size > current {
            let extra = size - current;
            if extra > self.max_bytes.saturating_sub(self.upper.usage().0) as u64 {
                return Err(FsError::NoSpace);
            }
        }
        self.upper.truncate(&path, size)
    }

    fn create(&self, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError> {
        let path = canonical(path);
        if self.visible(&path) {
            return Err(FsError::Exists);
        }
        if self.upper.usage().1 + 1 > self.max_nodes {
            return Err(FsError::NoSpace);
        }
        self.clear_whiteout(&path);
        self.upper.create(&path, mode, owner).map(upper_meta)
    }

    fn mkdir(&self, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError> {
        let path = canonical(path);
        if self.visible(&path) {
            return Err(FsError::Exists);
        }
        if self.upper.usage().1 + 1 > self.max_nodes {
            return Err(FsError::NoSpace);
        }
        self.clear_whiteout(&path);
        self.upper.mkdir(&path, mode, owner).map(upper_meta)
    }

    fn unlink(&self, path: &str) -> Result<(), FsError> {
        let path = canonical(path);
        match self.upper.lookup(&path) {
            Ok(meta) => {
                if meta.kind != FileKind::File {
                    return Err(FsError::IsDir);
                }
                self.upper.unlink(&path)?;
                // If a lower entry was shadowed, keep it hidden now that the
                // upper copy is gone so it does not resurface.
                if self.lower.lookup(&path).is_ok() {
                    self.whiteout(&path)?;
                }
                Ok(())
            }
            Err(FsError::NotFound) => match self.lower.lookup(&path) {
                Ok(meta) if meta.kind == FileKind::Dir => Err(FsError::IsDir),
                Ok(_) if !self.hidden(&path) => self.whiteout(&path),
                _ => Err(FsError::NotFound),
            },
            Err(error) => Err(error),
        }
    }

    fn rmdir(&self, path: &str) -> Result<(), FsError> {
        let path = canonical(path);
        let meta = self.lookup(&path)?;
        if meta.kind != FileKind::Dir {
            return Err(FsError::NotDir);
        }
        if !self.readdir(&path)?.is_empty() {
            return Err(FsError::NotEmpty);
        }
        match self.upper.lookup(&path) {
            Ok(_) => {
                self.upper.rmdir(&path)?;
            }
            Err(FsError::NotFound) => {}
            Err(error) => return Err(error),
        }
        if self.lower.lookup(&path).is_ok() {
            self.whiteout(&path)?;
        }
        Ok(())
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        let from = canonical(from);
        let to = canonical(to);
        if from == to {
            return Ok(());
        }
        if from.is_empty() || to.is_empty() {
            return Err(FsError::Access);
        }
        // Moving a directory below itself would make a cycle.
        if to.starts_with(&format!("{from}/")) {
            return Err(FsError::Invalid);
        }
        let source = self.lookup(&from)?;
        match self.lookup(&to) {
            Ok(victim) => {
                match (source.kind, victim.kind) {
                    (FileKind::File, FileKind::Dir) => return Err(FsError::IsDir),
                    (FileKind::Dir, FileKind::File) => return Err(FsError::NotDir),
                    (FileKind::Dir, FileKind::Dir) => {
                        // Replacing a directory requires it to be empty once
                        // the lower and upper layers are unioned.
                        let empty = self.readdir(&to)?.is_empty();
                        if !empty {
                            return Err(FsError::NotEmpty);
                        }
                    }
                    _ => {}
                }
            }
            Err(FsError::NotFound) => {}
            Err(error) => return Err(error),
        }

        // The destination parent must exist in the upper layer for the move.
        if let Some((parent, _)) = to.rsplit_once('/') {
            self.copy_up(parent)?;
        }
        self.copy_up(&from)?;
        self.upper.rename(&from, &to)?;
        // The lower source (if any) stays hidden after the move, and a lower
        // entry at the destination was replaced by the upper copy.
        if self.lower.lookup(&from).is_ok() {
            self.whiteout(&from)?;
        }
        if self.lower.lookup(&to).is_ok() {
            self.whiteout(&to)?;
        }
        Ok(())
    }

    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        let path = canonical(path);
        let meta = self.lookup(&path)?;
        if meta.kind != FileKind::Dir {
            return Err(FsError::NotDir);
        }
        let mut entries = match self.upper.readdir(&path) {
            Ok(entries) => entries
                .into_iter()
                .map(|entry| DirEntry {
                    ino: UPPER_INO | entry.ino,
                    ..entry
                })
                .collect(),
            Err(FsError::NotFound) => Vec::new(),
            Err(error) => return Err(error),
        };
        let mut names: BTreeSet<String> = entries.iter().map(|entry| entry.name.clone()).collect();
        if let Ok(lower) = self.lower.readdir(&path) {
            for entry in lower {
                let child = join(&path, &entry.name);
                if self.hidden(&child) || names.contains(&entry.name) {
                    continue;
                }
                names.insert(entry.name.clone());
                entries.push(entry);
            }
        }
        Ok(entries)
    }
}
