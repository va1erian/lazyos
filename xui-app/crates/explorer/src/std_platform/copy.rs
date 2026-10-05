#![forbid(unsafe_code)]

//! Copying dropped files and folders into a folder window's directory.
//!
//! The paths come from another app's drag, so the copy is careful: a folder
//! is never copied into itself or below itself, a name already taken gets
//! `name (2)` instead of being overwritten, symbolic links are copied as
//! links (never followed), and anything but files, folders and links is
//! skipped. One failure is reported and the rest still copy.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// What a copy did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CopyReport {
    /// Top-level items copied.
    pub copied: usize,
    /// What could not be copied, with why.
    pub failed: Vec<(PathBuf, String)>,
}

/// Copy each of `sources` into folder `dir`.
pub fn copy_into(sources: &[PathBuf], dir: &Path) -> CopyReport {
    let mut report = CopyReport::default();
    let dir_real = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    for source in sources {
        let outcome = (|| {
            let name = source
                .file_name()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
            let meta = fs::symlink_metadata(source)?;
            if meta.is_dir() {
                let real = fs::canonicalize(source)?;
                if dir_real.starts_with(&real) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "a folder cannot be copied into itself",
                    ));
                }
            }
            copy_entry(source, &unique(&dir.join(name)))
        })();
        match outcome {
            Ok(()) => report.copied += 1,
            Err(error) => report.failed.push((source.clone(), error.to_string())),
        }
    }
    report
}

/// Copy one file, link or folder (recursively) to `target`, which must not
/// exist.
fn copy_entry(source: &Path, target: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(source)?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        copy_link(source, target)
    } else if kind.is_dir() {
        fs::create_dir(target)?;
        let mut children: Vec<_> = fs::read_dir(source)?.collect::<io::Result<_>>()?;
        children.sort_by_key(|child| child.file_name());
        for child in children {
            copy_entry(&child.path(), &target.join(child.file_name()))?;
        }
        Ok(())
    } else if kind.is_file() {
        // `create_new`: never write through something already at `target`.
        let mut from = fs::File::open(source)?;
        let mut to = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        io::copy(&mut from, &mut to)?;
        to.set_permissions(meta.permissions())?;
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ))
    }
}

#[cfg(unix)]
fn copy_link(source: &Path, target: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(fs::read_link(source)?, target)
}

#[cfg(not(unix))]
fn copy_link(_source: &Path, _target: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "symbolic links are not supported here",
    ))
}

/// `path`, or `stem (2).ext`, `stem (3).ext`, ... beside it: the first free.
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
        _ => (name, String::new()),
    };
    (2u32..)
        .map(|n| path.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|candidate| fs::symlink_metadata(candidate).is_err())
        .unwrap_or_else(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("explorer-copy-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn files_and_folders_copy_and_names_never_collide() {
        let root = scratch("copy");
        fs::create_dir_all(root.join("src/tree/deep")).unwrap();
        fs::write(root.join("src/tree/deep/a.txt"), "a").unwrap();
        fs::write(root.join("src/note.txt"), "n").unwrap();
        let dest = root.join("dest");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("note.txt"), "mine").unwrap();
        let report = copy_into(&[root.join("src/tree"), root.join("src/note.txt")], &dest);
        assert_eq!(
            report,
            CopyReport {
                copied: 2,
                failed: Vec::new()
            }
        );
        assert_eq!(
            fs::read_to_string(dest.join("tree/deep/a.txt")).unwrap(),
            "a"
        );
        assert_eq!(fs::read_to_string(dest.join("note.txt")).unwrap(), "mine");
        assert_eq!(fs::read_to_string(dest.join("note (2).txt")).unwrap(), "n");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_folder_is_never_copied_into_itself() {
        let root = scratch("self");
        fs::create_dir_all(root.join("a/b")).unwrap();
        let report = copy_into(&[root.join("a")], &root.join("a/b"));
        assert_eq!(report.copied, 0);
        assert_eq!(report.failed.len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_missing_source_fails_alone() {
        let root = scratch("missing");
        fs::write(root.join("ok.txt"), "ok").unwrap();
        let dest = root.join("dest");
        fs::create_dir(&dest).unwrap();
        let report = copy_into(&[root.join("gone.txt"), root.join("ok.txt")], &dest);
        assert_eq!((report.copied, report.failed.len()), (1, 1));
        let _ = fs::remove_dir_all(root);
    }
}
