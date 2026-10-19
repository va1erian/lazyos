//! `rmdir` over unlinked-but-open files (issue #612), split out of the
//! parent module: files parked as hidden entries do not make a directory
//! non-empty.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use super::{hidden_name, Inode, OPEN};
use crate::fs::vfs::{FsError, Id};
/// `rmdir` of `path` (issue #612). On Linux an unlinked-but-open file has no
/// name left, so its directory is empty and can go; here the file is parked
/// as a hidden entry *in* that directory. So when the directory holds nothing
/// but parked files, they move to the root of the directory's mount first,
/// where they live until their last close (and where an unclean mount's
/// reclaim still finds them), and the directory is removed. If the removal
/// fails all the same (permissions, the sticky bit), they move back, so a
/// refused `rmdir` changes nothing.
///
/// A directory with any other entry, or a stray reserved name no open file
/// owns, is not empty: the backend answers `ENOTEMPTY` as before.
pub(in crate::fs) fn rmdir(id: Id, path: &str) -> Result<(), FsError> {
    let dir = crate::fs::nodes::abi_canonical(path);
    let parked = parked_in(&dir);
    let root = if parked.is_empty() {
        None
    } else {
        crate::fs::abi_with(|vfs| vfs.mount_point(&dir)).flatten()
    };
    let Some(root) = root.filter(|root| *root != dir && only(&dir, &parked)) else {
        return rmdir_raw(id, &dir);
    };
    let mut moved: Vec<(Arc<Inode>, String)> = Vec::new();
    for inode in parked {
        let from = inode.path();
        let to = hidden_name(&alloc::format!("{}/", root.trim_end_matches('/')));
        if crate::fs::abi_rename_raw(Id::ROOT, &from, &to).is_err() {
            unpark(moved);
            return rmdir_raw(id, &dir);
        }
        inode.set_key(to);
        moved.push((inode, from));
    }
    let result = rmdir_raw(id, &dir);
    if result.is_err() {
        unpark(moved);
    }
    result
}

fn rmdir_raw(id: Id, dir: &str) -> Result<(), FsError> {
    crate::fs::abi_with(|vfs| vfs.rmdir(id, dir)).unwrap_or(Err(FsError::NotFound))
}

/// The unlinked-but-open files parked directly inside `dir`.
fn parked_in(dir: &str) -> Vec<Arc<Inode>> {
    OPEN.lock()
        .iter()
        .filter(|inode| {
            inode.orphan.load(Ordering::Relaxed)
                && inode
                    .path()
                    .rsplit_once('/')
                    .is_some_and(|(parent, _)| parent == dir || (parent.is_empty() && dir == "/"))
        })
        .cloned()
        .collect()
}

/// Whether every entry of `dir` is one of the `parked` files.
fn only(dir: &str, parked: &[Arc<Inode>]) -> bool {
    let Ok(entries) = crate::fs::abi_readdir(Id::ROOT, dir) else {
        return false;
    };
    let names: Vec<String> = parked
        .iter()
        .filter_map(|inode| {
            inode
                .path()
                .rsplit_once('/')
                .map(|(_, name)| String::from(name))
        })
        .collect();
    entries.iter().all(|entry| names.contains(&entry.name))
}

/// Undo [`rmdir`]'s moves: each parked file goes back to its old hidden name.
fn unpark(moved: Vec<(Arc<Inode>, String)>) {
    for (inode, original) in moved {
        match crate::fs::abi_rename_raw(Id::ROOT, &inode.path(), &original) {
            Ok(()) => inode.set_key(original),
            // The file stays valid and reclaimable at its mount-root name; the
            // refused rmdir just cannot put it back.
            Err(error) => crate::serial_println!(
                "openfile: could not restore {original} after a refused rmdir: {error:?}"
            ),
        }
    }
}
