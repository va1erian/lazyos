//! Open files on the persistent mount, for Linux descriptors that read and
//! write the filesystem directly instead of a snapshot of it.
//!
//! A snapshot descriptor copies the file into the kernel heap at `open`, which
//! is fine for a few kilobytes on `/tmp` but wrong for a volume that outlives
//! the boot: the copy is bounded by the heap, and a second opener never sees the
//! first one's writes. An [`OpenFile`] holds a path and an offset, and every
//! read and write goes to the filesystem at that offset: by node where the
//! backend has nodes (ext2: the inode, resolved once at `open`,
//! `vfs/node.rs`), otherwise through the [`Vfs`](super::vfs::Vfs) by path.
//!
//! # Identity: one file, one entry
//!
//! All open descriptions of one file share one [`Inode`], with one page
//! cache, so a rename or unlink updates every one of them in one place, a
//! store through one shared mapping is seen through every other, and the last
//! one out knows it is the last. *One file* is decided by the file, not by
//! the name it was opened by (`registry.rs`): where the filesystem has nodes
//! (ext2, ramfs, a user-space filesystem), by the filesystem instance and the
//! node, so the two names of a hard-linked file, or `/tmp/x` and
//! `/transient/x`, open one [`Inode`]; on a filesystem without nodes (the
//! copy-up root, FAT), which cannot hold links, by the path.
//!
//! # POSIX behaviour the path alone cannot give
//!
//! The VFS names files by path, so a descriptor must keep meaning "the file I
//! opened" when its name changes:
//!
//! * `rename` moves the name, so every open file under the old path follows it
//!   ([`retarget`]).
//! * `unlink` must remove the *name* while the data stays reachable until the
//!   last close. An open file whose last name goes is therefore renamed to a
//!   hidden `.unlinked-<n>` entry in its directory instead of being freed
//!   ([`unlink_open`]), and that entry is deleted when the last descriptor
//!   goes ([`Drop`]). A stop between the two leaves the hidden entry behind, as
//!   an orphaned inode would on Linux; the next mount of an unclean volume
//!   deletes it (`Ext2::reclaim_orphans`). The name is reserved
//!   ([`hidden`](super::hidden)), so only this module ever creates one. A file
//!   that keeps another name (a hard link) is not parked: only the name goes,
//!   and the file goes on under its other names (issue #871).
//! * Renaming *over* an open file is an implicit unlink of it ([`displace`]).
//! * A directory holding nothing but parked files is empty to `rmdir`
//!   ([`rmdir`]), which first moves them to the root of the mount.
//!
//! # Permissions
//!
//! Access is decided when the file is opened (the caller checks the mode bits),
//! not at each read or write, as POSIX requires: a process that drops privilege
//! keeps the descriptors it already holds. Once open, operations therefore run
//! as [`Id::ROOT`], which also lets them walk a directory the caller can no
//! longer search.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

use super::vfs::{FsError, Id, Meta, Node, StatFs};

mod image;
mod mapping;
mod names;
mod registry;
mod unlink;
#[cfg(lazyos_tests)]
pub use names::hold_for_test;
#[cfg(lazyos_tests)]
pub use names::open_files;
pub use names::paths_under;
pub(in crate::fs) use names::{forget_name, retarget};
pub(crate) use registry::content_changed;
use registry::{find, release, share_inode};
pub(super) use unlink::{displace, names_lock, unlink_open, Displaced};
use unlink::{hidden_name, orphan};
mod rmdir;
use image::Role;
pub(in crate::fs) use image::{displace_text, refuse_text_write, rmdir_parked, unlink_text};
pub(super) use rmdir::rmdir;
pub mod pagecache;
#[cfg(lazyos_tests)]
pub use mapping::{deferred_pages, BEFORE_FLUSH};
pub use mapping::{writeback_all, MapError};
use pagecache::PageCache;

/// `path` in the one form the registry keys files by: through the first mount
/// of the filesystem holding it, so a filesystem mounted at two points (`/tmp`
/// and `/transient`) has one entry per file, not one per name (audit H6).
fn canonical(path: &str) -> String {
    super::nodes::abi_canonical(path)
}

/// The names of an open file.
struct Names {
    /// Absolute ABI path in [`canonical`] form, kept current across renames:
    /// the name operations on the file resolve. Kept, as the last name the
    /// file was known by (which mount it is on), once `current` is false.
    key: String,
    /// `key` still names this file. False once the name was given to another
    /// file or removed while the file kept another name (see
    /// [`share_inode`], [`forget_name`]).
    current: bool,
    /// The name the file is shown under (`/proc/self/fd`, the busy list of an
    /// eject): the one it was first opened by, which is `/tmp/x` while `key`
    /// says `/transient/x`. Kept current across renames; an unlinked file is
    /// shown under its hidden name.
    shown: String,
}

/// One file that has at least one open description; shared by all of them.
struct Inode {
    /// The file's names, behind one lock.
    names: Mutex<Names>,
    /// The name was unlinked while open: `path` is the hidden entry, to be
    /// deleted with the last description.
    orphan: AtomicBool,
    /// The file itself, where its filesystem has nodes: reads and writes go
    /// here and never resolve the path again.
    node: Option<Node>,
    /// Live open descriptions of this file, counted apart from the `Arc`'s
    /// references: `rmdir` and `displace` hold the inode briefly too, and a
    /// close racing with them must still see itself as the last description.
    /// Changed only under the [`OPEN`] lock.
    descriptions: AtomicUsize,
    /// Descriptions that run the file as a program's image, and descriptions
    /// open for writing (`image`): a file never has both. Changed only under
    /// the [`OPEN`] lock.
    text: AtomicUsize,
    writers: AtomicUsize,
    /// The frames a file-backed `mmap` maps (`mapping`).
    cache: PageCache,
    /// A write-back failure was reported (once per file).
    failure_logged: AtomicBool,
}

impl Inode {
    /// The name operations resolve, in [`canonical`] form; empty when the
    /// registry knows no current name of the file.
    fn path(&self) -> String {
        let names = self.names.lock();
        if names.current {
            names.key.clone()
        } else {
            String::new()
        }
    }

    /// [`Inode::path`] for an operation by name: a file the registry knows
    /// no current name of is [`FsError::NotFound`] (an empty path would
    /// name the root).
    fn name(&self) -> Result<String, FsError> {
        match self.path() {
            path if path.is_empty() => Err(FsError::NotFound),
            path => Ok(path),
        }
    }

    /// Whether `path` is the file's current name.
    fn is_named(&self, path: &str) -> bool {
        let names = self.names.lock();
        names.current && names.key == path
    }

    /// Give the file a new current name.
    fn set_key(&self, key: String) {
        let mut names = self.names.lock();
        names.key = key;
        names.current = true;
    }

    /// The name no longer names this file (keep it as the last one known).
    fn forget_key(&self) {
        self.names.lock().current = false;
    }

    /// The file's link count, from its node where it has one; `1` when it
    /// cannot be read (the file is then treated as having no other name).
    fn links(&self) -> u32 {
        let meta = match &self.node {
            Some(node) => node.stat(),
            None => self
                .name()
                .and_then(|path| super::abi_stat(Id::ROOT, &path)),
        };
        meta.map_or(1, |meta| meta.nlink)
    }

    /// The name to show: [`shown`](Self::shown), or the hidden name of an
    /// unlinked file.
    fn display(&self) -> String {
        if self.orphan.load(Ordering::Relaxed) {
            self.path()
        } else {
            self.names.lock().shown.clone()
        }
    }
}

/// Every file with an open description. Bounded by the descriptor tables
/// (`MAX_TASKS * limit.fd_max`) and in practice holds a few dozen files, so a
/// linear scan is cheap.
static OPEN: Mutex<Vec<Arc<Inode>>> = Mutex::new(Vec::new());

/// An open file description: what `open` returns and `dup`/`fork` share. The
/// offset lives here (not in the descriptor slot), so duplicated descriptors
/// move it together, as POSIX requires.
pub struct OpenFile {
    inode: Arc<Inode>,
    offset: AtomicU64,
    readable: bool,
    writable: bool,
    append: bool,
    /// What this description does to the file's `text`/`writers` counts.
    role: Role,
}

impl OpenFile {
    /// Open the regular file at `path` (already checked for access by the
    /// caller). Fails only when the registry cannot grow.
    pub fn open(
        path: &str,
        readable: bool,
        writable: bool,
        append: bool,
    ) -> Result<Arc<OpenFile>, FsError> {
        let shown = String::from(path);
        let path = canonical(path);
        // Access was checked by the caller; the node is opened as root.
        let node = super::nodes::abi_open_node(Id::ROOT, &path, 0)?;
        let role = if writable { Role::Writer } else { Role::Reader };
        let inode = share_inode(path, shown, node, role)?;
        Ok(Arc::new(OpenFile {
            inode,
            offset: AtomicU64::new(0),
            readable,
            writable,
            append,
            role,
        }))
    }

    pub fn readable(&self) -> bool {
        self.readable
    }

    pub fn writable(&self) -> bool {
        self.writable
    }

    pub fn append(&self) -> bool {
        self.append
    }

    /// `F_GETFL`: the access mode and `O_APPEND`, as Linux numbers them.
    pub fn status_flags(&self) -> u64 {
        const O_WRONLY: u64 = 1;
        const O_RDWR: u64 = 2;
        const O_APPEND: u64 = 0o2000;
        let access = match (self.readable, self.writable) {
            (true, true) => O_RDWR,
            (false, true) => O_WRONLY,
            _ => 0,
        };
        access | if self.append { O_APPEND } else { 0 }
    }

    /// The current absolute path in the registry's canonical form (the hidden
    /// name once unlinked): what operations on the file resolve, and what two
    /// names of one file have in common. Empty when no current name is known
    /// (the one it was opened by was removed while another name remains).
    pub fn path(&self) -> String {
        self.inode.path()
    }

    /// The last name the file was known by, in canonical form, current or
    /// not: which mount it lives on (who holds a volume busy).
    pub fn volume_path(&self) -> String {
        self.inode.names.lock().key.clone()
    }

    /// What advisory locks (`flock`, `fcntl`) are keyed by: the file itself
    /// where it has a node, so every name of one file shares its locks, and
    /// its path otherwise. The node form starts with a NUL, which no path
    /// holds.
    pub fn lock_key(&self) -> String {
        match &self.inode.node {
            Some(node) => {
                let id = node.id();
                alloc::format!("\0{:x}:{}:{}", node.fs_key(), id.ino, id.generation)
            }
            None => self.path(),
        }
    }

    /// The path to show a person or a program (`readlink` of the descriptor,
    /// who holds a volume busy): the name the file was opened by, kept
    /// current across renames, not the canonical alias.
    pub fn display_path(&self) -> String {
        self.inode.display()
    }

    pub fn offset(&self) -> u64 {
        self.offset.load(Ordering::Relaxed)
    }

    pub fn set_offset(&self, offset: u64) {
        self.offset.store(offset, Ordering::Relaxed);
    }

    /// Where a `write` lands: the end of the file for `O_APPEND`, otherwise the
    /// descriptor offset. Linux's `pwrite` also appends on an `O_APPEND` file.
    pub fn write_position(&self) -> Result<u64, FsError> {
        if self.append {
            Ok(self.stat()?.size)
        } else {
            Ok(self.offset())
        }
    }

    /// The file's metadata. An unlinked file parked under its hidden name
    /// counts that name out of `nlink`, so it reports `0` like an unlinked
    /// open file on Linux.
    pub fn stat(&self) -> Result<Meta, FsError> {
        let mut meta = match &self.inode.node {
            Some(node) => node.stat(),
            None => super::abi_stat(Id::ROOT, &self.inode.name()?),
        }?;
        if self.inode.orphan.load(Ordering::Relaxed) {
            meta.nlink = meta.nlink.saturating_sub(1);
        }
        Ok(meta)
    }

    /// Read at `offset`. A page a writable shared mapping may have changed
    /// is newer in its frame than in the file, so the frame's bytes win.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let count = self.read_raw(offset, buf)?;
        self.inode.cache.overlay(offset, &mut buf[..count]);
        Ok(count)
    }

    /// Read the file's own bytes, ignoring the page cache.
    fn read_raw(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        match &self.inode.node {
            Some(node) => node.read(offset, buf),
            None => super::abi_read_at(Id::ROOT, &self.inode.name()?, offset, buf),
        }
    }

    /// Write at `offset`. Through a node, the mount table's cached metadata
    /// of the path is refreshed afterwards, as a write by path does.
    pub fn write_at(&self, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        let Some(node) = &self.inode.node else {
            let written = super::abi_write(Id::ROOT, &self.inode.name()?, offset, data)?;
            self.inode.cache.update(offset, &data[..written]);
            return Ok(written);
        };
        let written = node.write(offset, data)?;
        self.inode.cache.update(offset, &data[..written]);
        let path = self.path();
        if !path.is_empty() {
            if let Ok(meta) = node.stat() {
                super::nodes::abi_refresh(&path, meta);
            }
        }
        Ok(written)
    }

    /// Set the file's size. Through a node this acts on the inode directly, so
    /// it still works after the name was taken by another file
    /// ([`share_inode`] clears the path then); the mount table's cached
    /// metadata of the path is refreshed afterwards, as a write by path does.
    pub fn truncate(&self, size: u64) -> Result<(), FsError> {
        let Some(node) = &self.inode.node else {
            super::abi_truncate(Id::ROOT, &self.inode.name()?, size)?;
            self.inode.truncated(size);
            return Ok(());
        };
        node.truncate(size)?;
        self.inode.truncated(size);
        let path = self.path();
        if !path.is_empty() {
            if let Ok(meta) = node.stat() {
                super::nodes::abi_refresh(&path, meta);
            }
        }
        Ok(())
    }

    /// Flush the filesystem this file lives on (`fsync`). Through a node, this
    /// does not depend on the path still naming the file.
    pub fn flush(&self) -> Result<(), FsError> {
        // Stores through a shared mapping are the file's data too.
        self.writeback(0, u64::MAX / pagecache::PAGE)?;
        match &self.inode.node {
            Some(node) => node.flush(),
            None => super::abi_flush(Id::ROOT, &self.inode.name()?, false),
        }
    }

    /// Flush the whole filesystem this file lives on (`syncfs`).
    pub fn flush_fs(&self) -> Result<(), FsError> {
        self.writeback(0, u64::MAX / pagecache::PAGE)?;
        match &self.inode.node {
            Some(node) => node.flush_fs(),
            None => super::abi_flush(Id::ROOT, &self.inode.name()?, true),
        }
    }

    pub fn statfs(&self) -> Result<StatFs, FsError> {
        match &self.inode.node {
            Some(node) => node.statfs(),
            None => super::abi_statfs(Id::ROOT, &self.inode.name()?),
        }
    }
}

impl Drop for OpenFile {
    /// The last description of an unlinked file frees it.
    fn drop(&mut self) {
        if release(&self.inode, self.role) && self.inode.orphan.load(Ordering::Relaxed) {
            let path = self.inode.path();
            if !path.is_empty() {
                let _ = super::abi_unlink_raw(Id::ROOT, &path);
            }
        }
    }
}
