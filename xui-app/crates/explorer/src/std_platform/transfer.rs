#![forbid(unsafe_code)]

//! What a drop into a folder does: copy or move, decided per item.
//!
//! The rule is the familiar desktop one, made safe for drops from other
//! apps: a drag that started in this explorer (`ours`) **moves** an item
//! that lives on the same volume as the folder and **copies** one from
//! another volume; Ctrl held at the drop forces a copy and Shift a move. A
//! drag from another app (the Archiver, say) always copies unless Shift is
//! held, so a drop never takes files away from an app that did not expect
//! it. An item dropped into the folder it already lives in is left alone.
//!
//! A move is a `rename`; when the kernel refuses it across volumes it falls
//! back to the careful copy ([`super::copy`]) followed by removing the
//! original, which happens only once the copy succeeded.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::copy::{CopyReport, copy_entry, copy_into, unique};

/// What happens to one dropped item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    Copy,
    Move,
}

/// What the drop knows about where it came from and the keys held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Intent {
    /// The drag started in this explorer.
    pub ours: bool,
    /// Ctrl was held at the drop: copy.
    pub ctrl: bool,
    /// Shift was held at the drop: move.
    pub shift: bool,
}

/// The transfer for one item, `same_volume` as the target folder or not.
pub fn choose(intent: Intent, same_volume: bool) -> Transfer {
    if intent.ctrl {
        Transfer::Copy
    } else if intent.shift || (intent.ours && same_volume) {
        Transfer::Move
    } else {
        Transfer::Copy
    }
}

/// What a drop did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DropReport {
    pub copied: usize,
    pub moved: usize,
    /// Items already in the folder, left alone.
    pub skipped: usize,
    pub failed: Vec<(PathBuf, String)>,
}

/// Drop each of `sources` into folder `dir` as `intent` decides.
pub fn drop_into(sources: &[PathBuf], dir: &Path, intent: Intent) -> DropReport {
    let mut report = DropReport::default();
    let dir_real = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    for source in sources {
        if already_in(source, &dir_real) {
            report.skipped += 1;
            continue;
        }
        match choose(intent, same_volume(source, dir)) {
            Transfer::Copy => {
                let CopyReport { copied, failed } = copy_into(std::slice::from_ref(source), dir);
                report.copied += copied;
                report.failed.extend(failed);
            }
            Transfer::Move => match move_one(source, dir, &dir_real) {
                Ok(()) => report.moved += 1,
                Err(error) => report.failed.push((source.clone(), error.to_string())),
            },
        }
    }
    report
}

/// Whether `source` already sits directly in the folder `dir_real`.
fn already_in(source: &Path, dir_real: &Path) -> bool {
    source
        .parent()
        .and_then(|parent| fs::canonicalize(parent).ok())
        .is_some_and(|parent| parent == dir_real)
}

/// Whether `source` and `dir` are on one filesystem.
#[cfg(unix)]
fn same_volume(source: &Path, dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::symlink_metadata(source), fs::metadata(dir)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_volume(_source: &Path, _dir: &Path) -> bool {
    false
}

/// Move one item into `dir` under a free name.
fn move_one(source: &Path, dir: &Path, dir_real: &Path) -> io::Result<()> {
    let name = source
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let meta = fs::symlink_metadata(source)?;
    if meta.is_dir() && dir_real.starts_with(fs::canonicalize(source)?) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a folder cannot be moved into itself",
        ));
    }
    let target = unique(&dir.join(name));
    match fs::rename(source, &target) {
        Ok(()) => Ok(()),
        Err(error) if crosses_volumes(&error) => {
            if let Err(error) = copy_entry(source, &target) {
                // Leave the original and take back the partial copy.
                let _ = remove(&target);
                return Err(error);
            }
            remove(source)
        }
        Err(error) => Err(error),
    }
}

/// Whether `rename` refused because source and target are on two volumes.
fn crosses_volumes(error: &io::Error) -> bool {
    /// Linux's `EXDEV`.
    const EXDEV: i32 = 18;
    error.raw_os_error() == Some(EXDEV)
}

/// Remove a file, link or whole folder.
fn remove(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("explorer-move-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const OURS: Intent = Intent {
        ours: true,
        ctrl: false,
        shift: false,
    };

    #[test]
    fn the_policy_moves_our_own_drags_on_one_volume() {
        let theirs = Intent::default();
        assert_eq!(choose(OURS, true), Transfer::Move);
        assert_eq!(choose(OURS, false), Transfer::Copy, "another volume");
        assert_eq!(choose(theirs, true), Transfer::Copy, "another app's drag");
        let ctrl = Intent { ctrl: true, ..OURS };
        assert_eq!(choose(ctrl, true), Transfer::Copy);
        let shift = Intent {
            shift: true,
            ..theirs
        };
        assert_eq!(choose(shift, false), Transfer::Move);
        let both = Intent {
            ctrl: true,
            shift: true,
            ..OURS
        };
        assert_eq!(choose(both, true), Transfer::Copy, "Ctrl wins");
    }

    #[test]
    fn a_move_takes_files_and_folders_without_clobbering() {
        // Shift: a move whatever the host says about volumes.
        let shift = Intent {
            shift: true,
            ..OURS
        };
        let root = scratch("move");
        fs::create_dir_all(root.join("a/tree/deep")).unwrap();
        fs::write(root.join("a/tree/deep/x.txt"), "x").unwrap();
        fs::write(root.join("a/note.txt"), "n").unwrap();
        fs::create_dir(root.join("b")).unwrap();
        fs::write(root.join("b/note.txt"), "mine").unwrap();
        let report = drop_into(
            &[root.join("a/tree"), root.join("a/note.txt")],
            &root.join("b"),
            shift,
        );
        assert_eq!(
            (report.moved, report.copied, report.failed.len()),
            (2, 0, 0)
        );
        assert!(!root.join("a/tree").exists() && !root.join("a/note.txt").exists());
        assert_eq!(
            fs::read_to_string(root.join("b/tree/deep/x.txt")).unwrap(),
            "x"
        );
        assert_eq!(fs::read_to_string(root.join("b/note.txt")).unwrap(), "mine");
        assert_eq!(
            fs::read_to_string(root.join("b/note (2).txt")).unwrap(),
            "n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ctrl_copies_and_another_apps_drag_copies() {
        let root = scratch("copy");
        fs::write(root.join("f.txt"), "f").unwrap();
        fs::create_dir(root.join("d")).unwrap();
        let ctrl = Intent { ctrl: true, ..OURS };
        let report = drop_into(&[root.join("f.txt")], &root.join("d"), ctrl);
        assert_eq!((report.copied, report.moved), (1, 0));
        let report = drop_into(&[root.join("f.txt")], &root.join("d"), Intent::default());
        assert_eq!((report.copied, report.moved), (1, 0));
        assert!(root.join("f.txt").exists(), "the original stays");
        assert!(root.join("d/f (2).txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_drop_into_its_own_folder_is_left_alone() {
        let root = scratch("same");
        fs::write(root.join("f.txt"), "f").unwrap();
        for intent in [OURS, Intent::default()] {
            let report = drop_into(&[root.join("f.txt")], &root, intent);
            assert_eq!((report.skipped, report.copied, report.moved), (1, 0, 0));
        }
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1, "no f (2).txt");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_folder_is_never_moved_into_itself() {
        let root = scratch("self");
        fs::create_dir_all(root.join("a/b")).unwrap();
        let shift = Intent {
            shift: true,
            ..OURS
        };
        let report = drop_into(&[root.join("a")], &root.join("a/b"), shift);
        assert_eq!((report.moved, report.failed.len()), (0, 1));
        assert!(root.join("a/b").is_dir());
        let _ = fs::remove_dir_all(root);
    }
}
