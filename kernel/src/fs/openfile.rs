//! Open files on the persistent mount, for Linux descriptors that read and
//! write the filesystem directly instead of a snapshot of it.
//!
//! A snapshot descriptor copies the file into the kernel heap at `open`, which
//! is fine for a few kilobytes on `/tmp` but wrong for a volume that outlives
//! the boot: the copy is bounded by the heap, and a second opener never sees the
//! first one's writes. An [`OpenFile`] holds only a path and an offset, and
//! every read and write goes to the [`Vfs`](super::vfs::Vfs) at that offset.
//!
//! # POSIX behaviour the path alone cannot give
//!
//! The VFS names files by path, so a descriptor must keep meaning "the file I
//! opened" when its name changes:
//!
//! * `rename` moves the name, so every open file under the old path follows it
//!   ([`retarget`]).
//! * `unlink` must remove the *name* while the data stays reachable until the
//!   last close. An open file is therefore renamed to a hidden
//!   `.unlinked-<n>` entry in its directory instead of being freed
//!   ([`unlink_open`]), and that entry is deleted when the last descriptor
//!   goes ([`Drop`]). A stop between the two leaves the hidden entry behind, as
//!   an orphaned inode would on Linux; the next mount of an unclean volume
//!   deletes it (`Ext2::reclaim_orphans`). The name is reserved
//!   ([`hidden`](super::hidden)), so only this module ever creates one.
//! * Renaming *over* an open file is an implicit unlink of it ([`displace`]).
//!
//! All open descriptions of one file share one [`Inode`], so a rename or unlink
//! updates every one of them in one place, and the last one out knows it is the
//! last.
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
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

use super::vfs::{FsError, Id, Meta, Path, StatFs};

/// One file that has at least one open description; shared by all of them.
struct Inode {
    /// Absolute ABI path, kept current across renames.
    path: Mutex<String>,
    /// The name was unlinked while open: `path` is the hidden entry, to be
    /// deleted with the last description.
    orphan: AtomicBool,
}

impl Inode {
    fn path(&self) -> String {
        self.path.lock().clone()
    }
}

/// Every file with an open description. Bounded by the descriptor tables
/// (`MAX_TASKS * limit.fd_max`) and in practice holds a few dozen files, so a
/// linear scan is cheap.
static OPEN: Mutex<Vec<Arc<Inode>>> = Mutex::new(Vec::new());

/// Source of unique hidden names; never reused within a boot.
static NEXT_ORPHAN: AtomicU64 = AtomicU64::new(1);

/// An open file description: what `open` returns and `dup`/`fork` share. The
/// offset lives here (not in the descriptor slot), so duplicated descriptors
/// move it together, as POSIX requires.
pub struct OpenFile {
    inode: Arc<Inode>,
    offset: AtomicU64,
    readable: bool,
    writable: bool,
    append: bool,
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
        let path = Path::parse(path).to_path_string();
        let inode = share_inode(path)?;
        Ok(Arc::new(OpenFile {
            inode,
            offset: AtomicU64::new(0),
            readable,
            writable,
            append,
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

    /// The current absolute path (the hidden name once unlinked).
    pub fn path(&self) -> String {
        self.inode.path()
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

    pub fn stat(&self) -> Result<Meta, FsError> {
        super::abi_stat(Id::ROOT, &self.path())
    }

    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        super::abi_read_at(Id::ROOT, &self.path(), offset, buf)
    }

    pub fn write_at(&self, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        super::abi_write(Id::ROOT, &self.path(), offset, data)
    }

    pub fn truncate(&self, size: u64) -> Result<(), FsError> {
        super::abi_truncate(Id::ROOT, &self.path(), size)
    }

    /// Flush the filesystem this file lives on (`fsync`).
    pub fn flush(&self) -> Result<(), FsError> {
        super::abi_flush(Id::ROOT, &self.path())
    }

    pub fn statfs(&self) -> Result<StatFs, FsError> {
        super::abi_statfs(Id::ROOT, &self.path())
    }
}

impl Drop for OpenFile {
    /// The last description of an unlinked file frees it.
    fn drop(&mut self) {
        if release(&self.inode) && self.inode.orphan.load(Ordering::Relaxed) {
            let _ = super::abi_unlink_raw(Id::ROOT, &self.inode.path());
        }
    }
}

/// The shared [`Inode`] for `path`, registering a new one if it is not open.
fn share_inode(path: String) -> Result<Arc<Inode>, FsError> {
    let mut open = OPEN.lock();
    if let Some(found) = open.iter().find(|inode| *inode.path.lock() == path) {
        return Ok(Arc::clone(found));
    }
    open.try_reserve(1).map_err(|_| FsError::NoSpace)?;
    let inode = Arc::new(Inode {
        path: Mutex::new(path),
        orphan: AtomicBool::new(false),
    });
    open.push(Arc::clone(&inode));
    Ok(inode)
}

/// Drop one description's claim on `inode`; true when it was the last, in which
/// case the inode leaves the registry.
fn release(inode: &Arc<Inode>) -> bool {
    let mut open = OPEN.lock();
    // The registry's own reference plus this description's.
    if Arc::strong_count(inode) > 2 {
        return false;
    }
    open.retain(|entry| !Arc::ptr_eq(entry, inode));
    true
}

/// The open inode whose current name is exactly `path`.
fn find(path: &str) -> Option<Arc<Inode>> {
    let path = Path::parse(path).to_path_string();
    let open = OPEN.lock();
    open.iter()
        .find(|inode| *inode.path.lock() == path)
        .cloned()
}

/// `dir/.unlinked-<n>` beside `path`, so the caller's rights to unlink `path`
/// are exactly the rights the hidden rename needs.
fn hidden_name(path: &str) -> String {
    let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
    let n = NEXT_ORPHAN.fetch_add(1, Ordering::Relaxed);
    alloc::format!("{dir}/{}{n}", super::hidden::PREFIX)
}

/// Take `inode`'s name away without freeing it: move it to a hidden entry.
/// Returns the hidden path.
fn orphan(id: Id, inode: &Inode) -> Result<String, FsError> {
    let from = inode.path();
    let hidden = hidden_name(&from);
    super::abi_rename_raw(id, &from, &hidden)?;
    *inode.path.lock() = hidden.clone();
    inode.orphan.store(true, Ordering::Relaxed);
    Ok(hidden)
}

/// `unlink` of `path`: if it is open, orphan it and report `true` (the caller
/// must not delete anything); if not, `false`.
pub(super) fn unlink_open(id: Id, path: &str) -> Result<bool, FsError> {
    match find(path) {
        Some(inode) => orphan(id, &inode).map(|_| true),
        None => Ok(false),
    }
}

/// A file pushed aside so a `rename` could replace its name.
pub(super) struct Displaced {
    inode: Arc<Inode>,
    hidden: String,
    original: String,
}

impl Displaced {
    /// The rename failed after all: put the file back under its name.
    pub(super) fn restore(self) {
        if super::abi_rename_raw(Id::ROOT, &self.hidden, &self.original).is_ok() {
            *self.inode.path.lock() = self.original;
            self.inode.orphan.store(false, Ordering::Relaxed);
        }
    }
}

/// Before `rename(_, to)` replaces `to`: if `to` is an open file, move it out of
/// the way (it is being unlinked) so its descriptors keep their data.
pub(super) fn displace(id: Id, to: &str) -> Result<Option<Displaced>, FsError> {
    let Some(inode) = find(to) else {
        return Ok(None);
    };
    let original = inode.path();
    let hidden = orphan(id, &inode)?;
    Ok(Some(Displaced {
        inode,
        hidden,
        original,
    }))
}

/// After a successful `rename(from, to)`: every open file at or under `from`
/// now lives at the same place under `to`.
pub(super) fn retarget(from: &str, to: &str) {
    let from = Path::parse(from).to_path_string();
    let to = Path::parse(to).to_path_string();
    for inode in OPEN.lock().iter() {
        let mut path = inode.path.lock();
        let Some(rest) = path.strip_prefix(from.as_str()) else {
            continue;
        };
        // Only a whole component matches: `/a/b` is not under `/a/bc`.
        if !rest.is_empty() && !rest.starts_with('/') {
            continue;
        }
        let itself = rest.is_empty();
        let moved = alloc::format!("{to}{rest}");
        *path = moved;
        // A hidden entry itself renamed back into the tree is a file again,
        // not garbage to delete on the last close. (A rename of an ancestor
        // directory leaves an orphan an orphan.)
        if itself {
            inode.orphan.store(false, Ordering::Relaxed);
        }
    }
}

/// How many files are registered as open; the leak tests assert it returns to
/// its baseline.
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn open_files() -> usize {
    OPEN.lock().len()
}
