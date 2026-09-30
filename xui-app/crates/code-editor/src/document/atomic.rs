#![forbid(unsafe_code)]

//! Atomic file writes for [`Document`](super::Document): a temp file in the
//! target's directory, then a rename. A symlink target is refused outright —
//! LazyOS never writes through a link — and on any failure the temp file is
//! removed.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::DocumentError;

/// A collision-resistant temp file name in `dir`.
fn temp_path(dir: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(".xui-notepad-{}-{n}.tmp", std::process::id()))
}

/// Writes `bytes` to `path` atomically: a temp file in the same directory is
/// written, flushed and synced, then renamed over the target.
///
/// A symlink target is refused, so a save can never be redirected outside the
/// path the user chose. On any failure the temp file is removed and the
/// original is untouched.
pub(super) fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), DocumentError> {
    let target = resolve(path).map_err(|source| DocumentError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let dir = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let permissions = fs::metadata(&target).ok().map(|meta| meta.permissions());
    let temp = temp_path(&dir);
    let result = write_temp_and_rename(&temp, &target, bytes, permissions);
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|source| DocumentError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// The file a write should reach: `path` itself, or an error when it is a
/// symlink (LazyOS never writes through a link).
fn resolve(path: &Path) -> io::Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing to write through a symlink",
        )),
        _ => Ok(path.to_path_buf()),
    }
}

/// The temp-file body of [`write_atomically`].
fn write_temp_and_rename(
    temp: &Path,
    target: &Path,
    bytes: &[u8],
    permissions: Option<fs::Permissions>,
) -> io::Result<()> {
    let mut file = fs::File::create(temp)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    if let Some(permissions) = permissions {
        fs::set_permissions(temp, permissions)?;
    }
    fs::rename(temp, target)
}
