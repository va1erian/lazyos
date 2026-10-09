#![forbid(unsafe_code)]

//! An in-memory [`Platform`] for tests that must not touch the real disk.
//!
//! Build a small tree with [`MemPlatform::file`], [`MemPlatform::dir`] and
//! [`MemPlatform::symlink`], then mark a folder unreadable with
//! [`MemPlatform::unreadable`] or a path undeletable with
//! [`MemPlatform::undeletable`] to exercise the failure paths.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io::{self, Error, ErrorKind};
use std::path::{Path, PathBuf};

use crate::model::is_within;
use crate::platform::{Kind, Meta, Platform, RawEntry};

/// An in-memory filesystem tree.
#[derive(Default)]
pub struct MemPlatform {
    inner: RefCell<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Every directory that exists, with its direct entries.
    dirs: HashMap<PathBuf, Vec<RawEntry>>,
    /// Directories whose `list` fails.
    unreadable: HashSet<PathBuf>,
    /// Paths whose `remove` fails.
    undeletable: HashSet<PathBuf>,
}

impl MemPlatform {
    /// An empty tree with `/` as its only (empty) directory.
    pub fn new() -> MemPlatform {
        MemPlatform::default()
    }

    /// Adds a regular file of `size` bytes at `path`, linked into its parent.
    pub fn file(self, path: &str, size: u64) -> MemPlatform {
        self.insert(path, Kind::File, Some(size))
    }

    /// Adds a directory at `path`, linked into its parent.
    pub fn dir(self, path: &str) -> MemPlatform {
        self.insert(path, Kind::Dir, None)
    }

    /// Adds a symlink at `path`, linked into its parent.
    pub fn symlink(self, path: &str) -> MemPlatform {
        self.insert(path, Kind::Symlink, None)
    }

    /// Adds a regular file to a tree already in use (a test's stand-in for
    /// a paste writing into the folder).
    pub fn add_file(&self, path: &Path, size: u64) {
        self.link(path.to_path_buf(), Kind::File, Some(size));
    }

    /// Adds an entry and ensures a containing directory record for it.
    fn insert(self, path: &str, kind: Kind, size: Option<u64>) -> MemPlatform {
        self.link(PathBuf::from(path), kind, size);
        self
    }

    /// Links `path` into its parent, creating the parent's record.
    fn link(&self, path: PathBuf, kind: Kind, size: Option<u64>) {
        {
            let mut inner = self.inner.borrow_mut();
            if kind == Kind::Dir {
                inner.dirs.entry(path.clone()).or_default();
            }
            if let Some(parent) = path.parent() {
                inner.dirs.entry(parent.to_path_buf()).or_default();
                let name = path.file_name().map(OsString::from).unwrap_or_default();
                let list = inner.dirs.get_mut(parent).expect("parent just inserted");
                list.retain(|entry| entry.name != name);
                list.push(RawEntry {
                    name: name.clone(),
                    meta: Meta {
                        name,
                        parent: Some(parent.to_path_buf()),
                        kind,
                        size,
                        modified: None,
                        created: None,
                        readonly: false,
                        entries: None,
                    },
                });
            }
        }
    }

    /// Makes `dir`'s `list` fail, as an unreadable folder would.
    pub fn unreadable(self, dir: &str) -> MemPlatform {
        self.inner
            .borrow_mut()
            .unreadable
            .insert(PathBuf::from(dir));
        self
    }

    /// Makes `dir`'s `list` fail (or succeed again) on a tree already in use,
    /// as a permission change would.
    pub fn set_readable(&self, dir: &str, readable: bool) {
        let mut inner = self.inner.borrow_mut();
        if readable {
            inner.unreadable.remove(Path::new(dir));
        } else {
            inner.unreadable.insert(PathBuf::from(dir));
        }
    }

    /// Makes `path`'s `remove` fail, as a permission error would.
    pub fn undeletable(self, path: &str) -> MemPlatform {
        self.inner
            .borrow_mut()
            .undeletable
            .insert(PathBuf::from(path));
        self
    }

    /// The entries still recorded for `dir`, for assertions after a delete.
    pub fn children(&self, dir: &str) -> Vec<OsString> {
        self.inner
            .borrow()
            .dirs
            .get(Path::new(dir))
            .map(|list| list.iter().map(|entry| entry.name.clone()).collect())
            .unwrap_or_default()
    }
}

impl Platform for MemPlatform {
    fn list(&self, dir: &Path) -> io::Result<Vec<RawEntry>> {
        let inner = self.inner.borrow();
        if inner.unreadable.contains(dir) {
            return Err(Error::new(ErrorKind::PermissionDenied, "permission denied"));
        }
        match inner.dirs.get(dir) {
            Some(list) => Ok(list.clone()),
            None => Err(Error::new(ErrorKind::NotFound, "not found")),
        }
    }

    fn metadata(&self, path: &Path) -> io::Result<Meta> {
        let inner = self.inner.borrow();
        let entry =
            find_entry(&inner, path).ok_or_else(|| Error::new(ErrorKind::NotFound, "not found"))?;
        let mut meta = entry.meta;
        if meta.kind == Kind::Dir {
            meta.entries = inner.dirs.get(path).map(Vec::len);
        }
        Ok(meta)
    }

    fn remove(&self, path: &Path, recursive: bool) -> io::Result<()> {
        let mut inner = self.inner.borrow_mut();
        if inner.undeletable.contains(path) {
            return Err(Error::new(ErrorKind::PermissionDenied, "permission denied"));
        }
        if inner.dirs.contains_key(path) {
            if !recursive && inner.dirs.get(path).is_some_and(|list| !list.is_empty()) {
                return Err(Error::new(ErrorKind::DirectoryNotEmpty, "not empty"));
            }
            let doomed: Vec<PathBuf> = inner
                .dirs
                .keys()
                .filter(|key| is_within(key, path))
                .cloned()
                .collect();
            for key in doomed {
                inner.dirs.remove(&key);
            }
            unlink(&mut inner, path);
            return Ok(());
        }
        if unlink(&mut inner, path) {
            Ok(())
        } else {
            Err(Error::new(ErrorKind::NotFound, "not found"))
        }
    }

    fn home(&self) -> Option<PathBuf> {
        Some(PathBuf::from("/home/user"))
    }
}

/// The entry `path` would appear as in its parent, if the parent is known.
fn find_entry(inner: &Inner, path: &Path) -> Option<RawEntry> {
    let parent = path.parent()?;
    let name = path.file_name()?;
    inner
        .dirs
        .get(parent)?
        .iter()
        .find(|entry| entry.name == name)
        .cloned()
}

/// Removes `path`'s entry from its parent's list, returning whether it was
/// there.
fn unlink(inner: &mut Inner, path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Some(name) = path.file_name() else {
        return false;
    };
    let Some(list) = inner.dirs.get_mut(parent) else {
        return false;
    };
    let before = list.len();
    list.retain(|entry| entry.name != name);
    list.len() != before
}
