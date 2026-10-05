//! Where an entry may be written: the rules that keep a hostile archive
//! inside the folder it is extracted to.
//!
//! * An entry whose stored name was absolute or contained `..` is never
//!   written ([`Entry::unsafe_path`](crate::Entry::unsafe_path)).
//! * Every directory between the destination and an entry is created by the
//!   extractor or must already be a real directory: a symlink in the way (one
//!   the archive itself just created, say) stops the entry, so nothing is
//!   written through a link.
//! * A symlink is created only when its target, resolved lexically from the
//!   link's own folder, stays inside the destination, and only after every
//!   file has been written.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Why an entry was not written.
pub type Refusal = String;

/// `relative` (a normalised archive path) below `root`, creating the
/// directories between them. Fails when one of them is not a real directory.
pub fn prepare(root: &Path, relative: &str) -> Result<PathBuf, Refusal> {
    let mut path = root.to_path_buf();
    let components: Vec<&str> = relative.split('/').collect();
    // Refuse before creating anything, so a bad name leaves no folders behind.
    if components.iter().any(|component| !is_plain(component)) {
        return Err("an unsafe path".to_owned());
    }
    let (leaf, parents) = components
        .split_last()
        .ok_or_else(|| "an empty path".to_owned())?;
    for component in parents {
        path.push(component);
        ensure_dir(&path)?;
    }
    path.push(leaf);
    Ok(path)
}

/// Make sure `path` is a real directory, creating it if missing.
pub fn ensure_dir(path: &Path) -> Result<(), Refusal> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() => Ok(()),
        Ok(meta) if meta.file_type().is_symlink() => Err("a symlink is in the way".to_owned()),
        Ok(_) => Err("a file is in the way".to_owned()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|error| error.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}

/// A component that names something inside its parent.
fn is_plain(component: &str) -> bool {
    !component.is_empty() && component != "." && component != ".." && !component.contains('\0')
}

/// Whether a symlink at `link` (a normalised archive path) pointing at
/// `target` stays inside the destination.
pub fn symlink_stays_inside(link: &str, target: &str) -> bool {
    if target.is_empty()
        || target.starts_with('/')
        || target.starts_with('\\')
        || target.contains('\0')
    {
        return false;
    }
    let mut depth: Vec<&str> = link.split('/').collect();
    depth.pop();
    for component in target.split(['/', '\\']) {
        match component {
            "" | "." => {}
            ".." => {
                if depth.pop().is_none() {
                    return false;
                }
            }
            other => depth.push(other),
        }
    }
    true
}

/// `path`, or `name (2).ext`, `name (3).ext`, ... beside it: the first that
/// does not exist.
pub fn unique(path: &Path) -> PathBuf {
    if fs::symlink_metadata(path).is_err() {
        return path.to_path_buf();
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_owned(), format!(".{ext}")),
        _ => (name.clone(), String::new()),
    };
    (2..)
        .map(|n| path.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|candidate| fs::symlink_metadata(candidate).is_err())
        .unwrap_or_else(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_must_resolve_inside() {
        assert!(symlink_stays_inside("a/link", "b.txt"));
        assert!(symlink_stays_inside("a/b/link", "../c"));
        assert!(!symlink_stays_inside("link", "../outside"));
        assert!(!symlink_stays_inside("a/link", "../../x"));
        assert!(!symlink_stays_inside("a/link", "/etc/passwd"));
        assert!(!symlink_stays_inside("a/link", ""));
    }

    #[test]
    fn traversal_components_are_refused() {
        let root = std::env::temp_dir();
        assert!(prepare(&root, "../x").is_err());
        assert!(prepare(&root, "a/../../x").is_err());
    }
}
