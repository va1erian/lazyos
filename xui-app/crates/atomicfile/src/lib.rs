#![forbid(unsafe_code)]

//! The one atomic file write of the desktop apps.
//!
//! A reader of the target sees the old bytes or the new ones, never half a
//! file, and a power cut cannot leave the new name on empty data: the temp
//! file is created exclusively next to the target (so the rename stays on one
//! filesystem and a planted link at a guessed name is never followed), written,
//! synced and given the target's permissions, renamed over it, and the
//! directory is synced so the rename itself reaches the disk. A target that is
//! a symlink is refused: LazyOS never writes through a link.
//!
//! On any failure the temp file is removed and the original is untouched.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// How many temp names a write tries before giving up.
const TEMP_TRIES: usize = 8;

/// Writes `bytes` to `path` atomically (see the module docs).
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_with(path, |file| file.write_all(bytes))
}

/// Like [`write`], with the body streamed by `fill` into the temp file (for a
/// writer that does not hold the whole file in memory).
pub fn write_with(
    path: &Path,
    fill: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> io::Result<()> {
    let permissions = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to write through a symlink",
            ));
        }
        Ok(meta) => Some(meta.permissions()),
        Err(_) => None,
    };
    let (temp, mut file) = create_temp(path)?;
    let result = fill(&mut file)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .and_then(|()| match permissions {
            Some(permissions) => fs::set_permissions(&temp, permissions),
            None => Ok(()),
        });
    drop(file);
    let result = result.and_then(|()| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
        return result;
    }
    sync_dir(path);
    Ok(())
}

/// A fresh temp file beside `target`. The name is only a guess at a free one:
/// `create_new` refuses whatever already sits there and the next is tried.
fn create_temp(target: &Path) -> io::Result<(PathBuf, fs::File)> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut last = None;
    for _ in 0..TEMP_TRIES {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut name = target
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        name.push(format!(".tmp-{}-{n}", std::process::id()));
        let temp = target.with_file_name(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(file) => return Ok((temp, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("no free temp name")))
}

/// Syncs `path`'s directory so the rename survives a power cut. Best effort:
/// a directory cannot be opened for this everywhere.
fn sync_dir(path: &Path) {
    #[cfg(unix)]
    {
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if let Ok(dir) = fs::File::open(dir) {
            let _ = dir.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("atomicfile-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn creates_and_replaces_without_leftovers() {
        let dir = scratch("replace");
        let path = dir.join("a.txt");
        write(&path, b"one").unwrap();
        write(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(names(&dir), ["a.txt"]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_directory_is_an_error() {
        let dir = scratch("missing");
        assert!(write(&dir.join("no/such/a.txt"), b"x").is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_fill_removes_the_temp_and_keeps_the_original() {
        let dir = scratch("fail");
        let path = dir.join("a.txt");
        write(&path, b"keep").unwrap();
        let error = write_with(&path, |_| Err(io::Error::other("boom"))).unwrap_err();
        assert_eq!(error.to_string(), "boom");
        assert_eq!(fs::read(&path).unwrap(), b"keep");
        assert_eq!(names(&dir), ["a.txt"]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_squatted_temp_name_is_skipped() {
        let dir = scratch("squat");
        let path = dir.join("a.txt");
        for n in 0..3 {
            let squat = dir.join(format!("a.txt.tmp-{}-{n}", std::process::id()));
            fs::write(squat, b"mine").unwrap();
        }
        write(&path, b"ok").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"ok");
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_target_is_refused_and_permissions_are_kept() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = scratch("link");
        let real = dir.join("real");
        fs::write(&real, b"r").unwrap();
        symlink(&real, dir.join("link")).unwrap();
        let error = write(&dir.join("link"), b"x").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&real).unwrap(), b"r");
        fs::set_permissions(&real, fs::Permissions::from_mode(0o640)).unwrap();
        write(&real, b"n").unwrap();
        let mode = fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
        let _ = fs::remove_dir_all(dir);
    }
}
