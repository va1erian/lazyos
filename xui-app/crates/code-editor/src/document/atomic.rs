#![forbid(unsafe_code)]

//! Atomic file writes for [`Document`](super::Document), through the shared
//! `atomicfile` helper (temp file, rename; a symlink target is refused).

use std::path::Path;

use super::DocumentError;

/// Writes `bytes` to `path` atomically; on any failure the original is
/// untouched.
pub(super) fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), DocumentError> {
    atomicfile::write(path, bytes).map_err(|source| DocumentError::Io {
        path: path.to_path_buf(),
        source,
    })
}
