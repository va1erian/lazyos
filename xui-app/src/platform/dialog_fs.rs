//! The [`FileSystem`] the Editor's Open / Save As dialog lists through.
//!
//! LazyOS mounts `/tmp` (ramfs) and `/data` (ext2) over the FAT boot volume,
//! and the VFS does not synthesise a mount point's entry in its parent's
//! directory listing: `read_dir("/")` shows the FAT files only, so the dialog
//! could never see, let alone navigate into, the writable volumes. This wrapper
//! lists through the Linux shim's `std::fs` and adds the mount points that are
//! reachable but missing from the listing.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_core::widget::{Entry, FileSystem, StdFileSystem};

/// Mount points that hang off `/` (see `docs/architecture/filesystem.md`).
const ROOT_MOUNTS: &[&str] = &["tmp", "data"];

/// A [`FileSystem`] over `inner` that surfaces the root mount points.
pub struct LazyFileSystem<F: FileSystem = StdFileSystem> {
    inner: F,
}

impl LazyFileSystem<StdFileSystem> {
    /// The dialog's filesystem over the shim's `std::fs`, ready for
    /// `FileDialog::file_system`.
    pub fn shared() -> Rc<dyn FileSystem> {
        Rc::new(LazyFileSystem {
            inner: StdFileSystem,
        })
    }
}

impl<F: FileSystem> LazyFileSystem<F> {
    /// A wrapper over `inner`.
    pub fn new(inner: F) -> LazyFileSystem<F> {
        LazyFileSystem { inner }
    }
}

impl<F: FileSystem> FileSystem for LazyFileSystem<F> {
    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        let mut entries = self.inner.list(dir)?;
        if dir == Path::new("/") {
            for mount in ROOT_MOUNTS {
                let name = OsString::from(*mount);
                let listed = entries
                    .iter()
                    .any(|entry| entry.name.to_string_lossy().eq_ignore_ascii_case(mount));
                if !listed && self.inner.is_dir(&PathBuf::from("/").join(mount)) {
                    entries.push(Entry {
                        name,
                        is_dir: true,
                        size: None,
                        modified: None,
                    });
                }
            }
        }
        Ok(entries)
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.inner.is_dir(path)
    }

    fn exists(&self, path: &Path) -> bool {
        self.inner.exists(path)
    }

    fn home(&self) -> Option<PathBuf> {
        self.inner.home()
    }

    fn roots(&self) -> Vec<PathBuf> {
        self.inner.roots()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A canned root listing with the given directories present.
    struct Fake {
        entries: Vec<&'static str>,
        dirs: Vec<&'static str>,
    }

    impl FileSystem for Fake {
        fn list(&self, _dir: &Path) -> io::Result<Vec<Entry>> {
            Ok(self
                .entries
                .iter()
                .map(|name| Entry {
                    name: (*name).into(),
                    is_dir: false,
                    size: Some(1),
                    modified: None,
                })
                .collect())
        }
        fn is_dir(&self, path: &Path) -> bool {
            self.dirs.iter().any(|dir| Path::new(dir) == path)
        }
        fn exists(&self, path: &Path) -> bool {
            self.is_dir(path)
        }
        fn home(&self) -> Option<PathBuf> {
            None
        }
        fn roots(&self) -> Vec<PathBuf> {
            vec![PathBuf::from("/")]
        }
    }

    fn names(fs: &impl FileSystem, dir: &str) -> Vec<String> {
        let mut names: Vec<String> = fs
            .list(Path::new(dir))
            .unwrap()
            .into_iter()
            .map(|entry| entry.name.to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn root_gains_the_mount_points_that_exist() {
        let fs = LazyFileSystem::new(Fake {
            entries: vec!["HELLO.TXT"],
            dirs: vec!["/tmp"],
        });
        assert_eq!(names(&fs, "/"), ["HELLO.TXT", "tmp"]);
    }

    #[test]
    fn a_mount_point_is_never_listed_twice() {
        let fs = LazyFileSystem::new(Fake {
            entries: vec!["TMP", "data"],
            dirs: vec!["/tmp", "/data"],
        });
        assert_eq!(names(&fs, "/"), ["TMP", "data"]);
    }

    #[test]
    fn only_the_root_is_augmented() {
        let fs = LazyFileSystem::new(Fake {
            entries: vec!["a"],
            dirs: vec!["/tmp", "/data"],
        });
        assert_eq!(names(&fs, "/tmp"), ["a"]);
    }

    #[test]
    fn the_added_entries_are_directories() {
        let fs = LazyFileSystem::new(Fake {
            entries: vec![],
            dirs: vec!["/data"],
        });
        let entries = fs.list(Path::new("/")).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_dir);
    }
}
