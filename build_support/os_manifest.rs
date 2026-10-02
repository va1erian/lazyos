//! The manifest of build-placed paths (`/system/.image-manifest`): the only
//! record of what an update may replace or delete. A path in neither the old
//! nor the new manifest is never touched, which is what keeps `/apps`, `/conf`,
//! `/logs`, `/data` contents and anything a user created across a rebuild.

use std::collections::BTreeMap;

use crate::os_image::OsFile;
use crate::os_layout::{DirSpec, MANIFEST_PATH};

/// `path` as an absolute, normalised path, or `None` when it has an empty, `.`
/// or `..` component or a character a manifest line cannot carry.
pub fn clean_path(path: &str) -> Option<String> {
    if path.contains(['\n', '\r', '\0']) {
        return None;
    }
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() || parts.iter().any(|part| *part == "." || *part == "..") {
        return None;
    }
    Some(format!("/{}", parts.join("/")))
}

/// Whether a manifest entry is a directory or a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Dir,
    File,
}

/// Every path the build placed: files and the directories it created.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    pub entries: BTreeMap<String, Kind>,
}

impl Manifest {
    /// The manifest of a build: the layout directories, the files, and the
    /// parent directories the files need.
    pub fn of(dirs: &[DirSpec], files: &[OsFile]) -> Result<Manifest, String> {
        let mut entries = BTreeMap::new();
        let mut put = |path: &str, kind: Kind| match entries.insert(path.to_string(), kind) {
            Some(old) if old != kind => Err(format!("{path} is both a file and a directory")),
            _ => Ok(()),
        };
        for dir in dirs {
            put(&dir.path, Kind::Dir)?;
        }
        for file in files {
            let mut at = 0;
            while let Some(next) = file.path[at + 1..].find('/') {
                at += 1 + next;
                put(&file.path[..at], Kind::Dir)?;
            }
            put(&file.path, Kind::File)?;
        }
        Ok(Manifest { entries })
    }

    /// One `d <path>` or `f <path>` line per entry, sorted.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        for (path, kind) in &self.entries {
            text.push(if *kind == Kind::Dir { 'd' } else { 'f' });
            text.push(' ');
            text.push_str(path);
            text.push('\n');
        }
        text
    }

    /// The inverse of [`Manifest::to_text`]. The text came off a disk, so a
    /// line that is not `d`/`f` plus a clean path rejects the whole manifest.
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let mut entries = BTreeMap::new();
        for (number, line) in text.lines().enumerate() {
            let (kind, path) = match line.split_once(' ') {
                Some(("d", path)) => (Kind::Dir, path),
                Some(("f", path)) => (Kind::File, path),
                _ => return Err(format!("manifest line {} is malformed", number + 1)),
            };
            if clean_path(path).as_deref() != Some(path) || path == MANIFEST_PATH {
                return Err(format!("manifest line {} has a bad path", number + 1));
            }
            entries.insert(path.to_string(), kind);
        }
        Ok(Manifest { entries })
    }

    /// The entries of `self` (the old manifest) that `new` no longer lists, or
    /// lists as the other kind, deepest first so a directory follows its
    /// contents.
    pub fn removed_by(&self, new: &Manifest) -> Vec<(String, Kind)> {
        let mut gone: Vec<(String, Kind)> = self
            .entries
            .iter()
            .filter(|(path, kind)| new.entries.get(*path) != Some(*kind))
            .map(|(path, kind)| (path.clone(), *kind))
            .collect();
        gone.reverse();
        gone
    }
}
