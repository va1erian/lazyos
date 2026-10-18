//! The registry's names: following a rename, and asking which files are open
//! at or under a path. Split out of the parent module for the file-size limit.

use alloc::string::String;
use core::sync::atomic::Ordering;

#[cfg(lazyos_tests)]
use super::find;
use super::{canonical, OPEN};

/// After a successful `rename(from, to)`: every open file at or under `from`
/// now lives at the same place under `to`.
pub(in crate::fs) fn retarget(raw_from: &str, raw_to: &str) {
    let from = canonical(raw_from);
    let to = canonical(raw_to);
    for inode in OPEN.lock().iter() {
        let mut guard = inode.names.lock();
        let names = &mut *guard;
        let Some(rest) = names.key.strip_prefix(from.as_str()) else {
            continue;
        };
        // Only a whole component matches: `/a/b` is not under `/a/bc`.
        if !rest.is_empty() && !rest.starts_with('/') {
            continue;
        }
        let itself = rest.is_empty();
        let moved = alloc::format!("{to}{rest}");
        // The shown name follows in the alias it is shown in; one that was
        // opened by another alias of the filesystem takes the renamer's.
        let below = names
            .shown
            .strip_prefix(raw_from)
            .filter(|tail| tail.is_empty() || tail.starts_with('/'));
        let renamed = alloc::format!("{raw_to}{}", below.unwrap_or(rest));
        names.shown = renamed;
        names.key = moved;
        // A hidden entry itself renamed back into the tree is a file again,
        // not garbage to delete on the last close. (A rename of an ancestor
        // directory leaves an orphan an orphan.)
        if itself {
            inode.orphan.store(false, Ordering::Relaxed);
        }
    }
}

/// Test hook: a transient reference to the open inode named `path`, the way
/// `rmdir` holds one while it moves parked files (issue #612).
#[cfg(lazyos_tests)]
pub fn hold_for_test(path: &str) -> Option<alloc::boxed::Box<dyn core::any::Any>> {
    find(path).map(|inode| alloc::boxed::Box::new(inode) as alloc::boxed::Box<dyn core::any::Any>)
}

/// How many files are registered as open; the leak tests assert it returns to
/// its baseline.
#[cfg(lazyos_tests)]
pub fn open_files() -> usize {
    OPEN.lock().len()
}

/// The names of every open file at or under `prefix` (whole components).
/// Mappings keep a file registered after its descriptor closed, so this sees
/// what a descriptor scan of the task table does not.
pub fn paths_under(prefix: &str) -> alloc::vec::Vec<String> {
    let prefix = canonical(prefix);
    let mut found = alloc::vec::Vec::new();
    for inode in OPEN.lock().iter() {
        // Looked at in place: only a match is cloned, and only so many.
        let names = inode.names.lock();
        if crate::task::holders::is_under(&names.key, &prefix) {
            found.push(if inode.orphan.load(Ordering::Relaxed) {
                names.key.clone()
            } else {
                names.shown.clone()
            });
            if found.len() >= crate::task::holders::MAX_HOLDERS {
                break;
            }
        }
    }
    found
}
