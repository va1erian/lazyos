#![forbid(unsafe_code)]

//! [`StdPlatform`]: the [`Platform`] implementation over `std::fs`.
//!
//! Deletion and measurement use `symlink_metadata`, so a symlink is reported and
//! removed as a link, never followed.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::platform::{Kind, Meta, Platform, RawEntry};

/// The desktop filesystem.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdPlatform;

impl StdPlatform {
    /// The platform.
    pub const fn new() -> StdPlatform {
        StdPlatform
    }
}

/// `path`'s metadata, with a directory's entry count when `count` says so.
fn stat(path: &Path, count: bool) -> io::Result<Meta> {
    let attributes = fs::symlink_metadata(path)?;
    let file_type = attributes.file_type();
    let kind = if file_type.is_symlink() {
        Kind::Symlink
    } else if file_type.is_dir() {
        Kind::Dir
    } else {
        Kind::File
    };
    let mut meta = Meta::bare(path, kind);
    if kind != Kind::Dir {
        meta.size = Some(attributes.len());
    } else if count {
        meta.entries = fs::read_dir(path)
            .ok()
            .map(|entries| entries.filter_map(Result::ok).count());
    }
    meta.modified = attributes.modified().ok();
    meta.created = attributes.created().ok();
    meta.readonly = attributes.permissions().readonly();
    Ok(meta)
}

impl Platform for StdPlatform {
    fn list(&self, dir: &Path) -> io::Result<Vec<RawEntry>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let path = entry.path();
            // An entry that vanished between the read and the stat is skipped
            // rather than failing the whole listing. A folder's own entries
            // are not counted here: that is one more directory read per
            // subfolder (a remote listing on a network mount, on the UI
            // thread), and only the properties dialog shows the count.
            if let Ok(meta) = stat(&path, false) {
                entries.push(RawEntry { name, meta });
            }
        }
        Ok(entries)
    }

    fn metadata(&self, path: &Path) -> io::Result<Meta> {
        stat(path, true)
    }

    fn remove(&self, path: &Path, recursive: bool) -> io::Result<()> {
        let attributes = fs::symlink_metadata(path)?;
        let file_type = attributes.file_type();
        if file_type.is_symlink() || !file_type.is_dir() {
            fs::remove_file(path)
        } else if recursive {
            fs::remove_dir_all(path)
        } else {
            fs::remove_dir(path)
        }
    }

    fn home(&self) -> Option<PathBuf> {
        home_dir()
    }
}

/// The user's home directory from the conventional environment variables.
fn home_dir() -> Option<PathBuf> {
    for key in ["HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(key)
            && !value.is_empty()
        {
            return Some(PathBuf::from(value));
        }
    }
    None
}
