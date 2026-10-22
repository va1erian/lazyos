//! Taking a name away from an open file: `unlink`, and a `rename` over it.
//!
//! A file losing its *last* name while open is parked under a hidden
//! `.unlinked-<n>` name until its last close (the parent module's docs). A
//! file that keeps another name (a hard link, issue #871) is not: only the
//! name goes, the file goes on under its other names, and its link count
//! says so (`st_nlink` 1 afterwards, which is what WebKit's blob cache reads
//! to tell an unreferenced blob).
//!
//! Whether a file has another name is decided and acted on under a lock of
//! the filesystem holding it, [`names_lock`], which every `unlink`, `rename`
//! and `link` holds: two unlinks of a file's two names racing would otherwise
//! each see the other name and both remove theirs, freeing a file that is
//! still open. Only filesystems that can hold links take one (a user-space
//! filesystem's round trip never holds up another mount), and filesystems
//! share a few locks by their address.

use alloc::string::String;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use super::{canonical, find, forget_name, Inode};
use crate::fs::vfs::{FsError, Id};
use crate::task::relax::{YieldGuard, YieldMutex};

/// Source of unique hidden names; never reused within a boot.
static NEXT_ORPHAN: AtomicU64 = AtomicU64::new(1);

/// Held while a name is taken from a file that may be open (see the module
/// docs), one per filesystem modulo their number. Taken before either mount
/// table's lock, never inside one, and never two at once.
static NAMES: [YieldMutex<()>; 8] = [const { YieldMutex::named("open-names", ()) }; 8];

/// Hold the lock of the filesystem holding `path` for the length of one name
/// change; `None` for a filesystem whose files have no stable nodes (no links
/// there, nothing to decide) or a path no mount holds. The path is looked up
/// in the Linux ABI table, native callers' too: like [`canonical`], this
/// assumes both tables mount the same instances at the same points (the
/// configured layout), or a native and a Linux unlink of one file could take
/// different locks.
pub(in crate::fs) fn names_lock(path: &str) -> Option<YieldGuard<'static, ()>> {
    let (fs, stable) = crate::fs::nodes::abi_fs_of(path)?;
    // The instance's address, past its alignment bits, picks the lock.
    stable.then(|| NAMES[(fs >> 4) % NAMES.len()].lock())
}

/// `dir/.unlinked-<n>` beside `path`, so the caller's rights to unlink `path`
/// are exactly the rights the hidden rename needs.
pub(super) fn hidden_name(path: &str) -> String {
    let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
    let n = NEXT_ORPHAN.fetch_add(1, Ordering::Relaxed);
    alloc::format!("{dir}/{}{n}", crate::fs::hidden::PREFIX)
}

/// Take the name `path`, the last one of the open `inode`, away without
/// freeing the file: move it to a hidden entry. Returns the hidden path.
pub(super) fn orphan(id: Id, inode: &Inode, path: &str) -> Result<String, FsError> {
    let from = canonical(path);
    let hidden = hidden_name(&from);
    crate::fs::abi_rename_raw(id, &from, &hidden)?;
    inode.set_key(hidden.clone());
    inode.orphan.store(true, Ordering::Relaxed);
    Ok(hidden)
}

/// `unlink` of `path` (the caller holds [`names_lock`]). An open file is
/// handled here and `true` returned: its last name is parked, another name
/// is removed outright. `false` when the file is not open: the caller
/// deletes the name itself.
pub(in crate::fs) fn unlink_open(id: Id, path: &str) -> Result<bool, FsError> {
    let Some(inode) = find(path) else {
        return Ok(false);
    };
    if inode.links() > 1 {
        crate::fs::abi_unlink_raw(id, path)?;
        forget_name(path);
        return Ok(true);
    }
    orphan(id, &inode, path).map(|_| true)
}

/// A file a `rename` replaced or is about to replace.
pub(in crate::fs) struct Displaced {
    inode: Arc<Inode>,
    /// Where the file was parked, when the name was its last one.
    parked: Option<String>,
    /// The name the rename takes, in canonical form.
    original: String,
    /// `from` and `to` are two names of this one file: the rename is a no-op.
    same_file: bool,
}

impl Displaced {
    /// The rename was between two names of one file, a no-op: nothing to
    /// settle and no name to follow.
    pub(in crate::fs) fn same_file(&self) -> bool {
        self.same_file
    }

    /// The rename ended (`renamed` says how). A parked file goes back under
    /// its name when the rename failed; a file that kept another name loses
    /// this one when it succeeded.
    pub(in crate::fs) fn settle(self, renamed: bool) {
        if self.same_file {
            return;
        }
        match (self.parked, renamed) {
            (Some(hidden), false) => {
                if crate::fs::abi_rename_raw(Id::ROOT, &hidden, &self.original).is_ok() {
                    self.inode.set_key(self.original);
                    self.inode.orphan.store(false, Ordering::Relaxed);
                }
            }
            (None, true) if self.inode.is_named(&self.original) => {
                self.inode.forget_key();
            }
            _ => {}
        }
    }
}

/// Before `rename(from, to)` replaces `to` (the caller holds [`names_lock`]):
/// if `to` is an open file it is being unlinked, so its last name is parked
/// and its descriptors keep their data. Two names of one file are no
/// replacement at all (the rename is a no-op, [`Displaced::same_file`]), and a
/// file with another name left only loses this one.
pub(in crate::fs) fn displace(id: Id, from: &str, to: &str) -> Result<Option<Displaced>, FsError> {
    let Some(inode) = find(to) else {
        return Ok(None);
    };
    let original = canonical(to);
    if find(from).is_some_and(|source| Arc::ptr_eq(&source, &inode)) {
        return Ok(Some(Displaced {
            inode,
            parked: None,
            original,
            same_file: true,
        }));
    }
    if inode.links() > 1 {
        return Ok(Some(Displaced {
            inode,
            parked: None,
            original,
            same_file: false,
        }));
    }
    let hidden = orphan(id, &inode, to)?;
    Ok(Some(Displaced {
        inode,
        parked: Some(hidden),
        original,
        same_file: false,
    }))
}
