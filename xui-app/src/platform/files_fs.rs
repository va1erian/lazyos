//! The Files app's [`Platform`]: `std::fs` plus the root mount points.
//!
//! Same VFS gap as [`super::dialog_fs`]: `read_dir("/")` omits `/tmp` and
//! `/data`, the mount points over the FAT volume. This wrapper adds those that
//! resolve as directories so Files can browse into the writable volumes.

use std::io;
use std::path::{Path, PathBuf};

use xui_explorer::platform::{Kind, Meta, Platform, RawEntry};

use super::dialog_fs::ROOT_MOUNTS;

/// A [`Platform`] over `inner` that lists the root mount points.
pub struct LazyPlatform<P: Platform> {
    inner: P,
}

impl<P: Platform> LazyPlatform<P> {
    /// A wrapper over `inner`.
    pub fn new(inner: P) -> LazyPlatform<P> {
        LazyPlatform { inner }
    }
}

impl<P: Platform> Platform for LazyPlatform<P> {
    fn list(&self, dir: &Path) -> io::Result<Vec<RawEntry>> {
        let mut entries = self.inner.list(dir)?;
        if dir == Path::new("/") {
            for mount in ROOT_MOUNTS {
                let listed = entries
                    .iter()
                    .any(|entry| entry.name.to_string_lossy().eq_ignore_ascii_case(mount));
                if listed {
                    continue;
                }
                let path = PathBuf::from("/").join(mount);
                if let Ok(meta) = self.inner.metadata(&path) {
                    if meta.kind == Kind::Dir {
                        entries.push(RawEntry {
                            name: (*mount).into(),
                            meta,
                        });
                    }
                }
            }
        }
        Ok(entries)
    }

    fn metadata(&self, path: &Path) -> io::Result<Meta> {
        self.inner.metadata(path)
    }

    fn remove(&self, path: &Path, recursive: bool) -> io::Result<()> {
        self.inner.remove(path, recursive)
    }

    fn home(&self) -> Option<PathBuf> {
        self.inner.home()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake whose root lists `files` and whose `dirs` resolve as directories.
    struct Fake {
        files: Vec<&'static str>,
        dirs: Vec<&'static str>,
    }

    impl Platform for Fake {
        fn list(&self, _dir: &Path) -> io::Result<Vec<RawEntry>> {
            Ok(self
                .files
                .iter()
                .map(|name| RawEntry {
                    name: (*name).into(),
                    meta: Meta::bare(&Path::new("/").join(name), Kind::File),
                })
                .collect())
        }
        fn metadata(&self, path: &Path) -> io::Result<Meta> {
            if self.dirs.iter().any(|dir| Path::new(dir) == path) {
                Ok(Meta::bare(path, Kind::Dir))
            } else {
                Err(io::ErrorKind::NotFound.into())
            }
        }
        fn remove(&self, _path: &Path, _recursive: bool) -> io::Result<()> {
            Ok(())
        }
        fn home(&self) -> Option<PathBuf> {
            None
        }
    }

    fn names(platform: &impl Platform, dir: &str) -> Vec<String> {
        let mut names: Vec<String> = platform
            .list(Path::new(dir))
            .unwrap()
            .into_iter()
            .map(|entry| entry.name.to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn root_lists_the_mounts_that_exist() {
        let platform = LazyPlatform::new(Fake {
            files: vec!["HELLO.TXT"],
            dirs: vec!["/tmp"],
        });
        assert_eq!(names(&platform, "/"), ["HELLO.TXT", "tmp"]);
    }

    #[test]
    fn a_listed_mount_is_not_duplicated() {
        let platform = LazyPlatform::new(Fake {
            files: vec!["tmp"],
            dirs: vec!["/tmp"],
        });
        assert_eq!(names(&platform, "/"), ["tmp"]);
    }

    #[test]
    fn other_directories_are_untouched() {
        let platform = LazyPlatform::new(Fake {
            files: vec!["x"],
            dirs: vec!["/tmp", "/data"],
        });
        assert_eq!(names(&platform, "/tmp"), ["x"]);
    }
}
