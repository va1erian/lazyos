#![forbid(unsafe_code)]

//! The optional persistence seam.
//!
//! The app never calls a filesystem directly: it asks its [`Storage`] whether it
//! is [`available`](Storage::available) and shows the Save/Open buttons only if
//! so. A failing save or load is reported in the status bar and never panics, so
//! the app works with nothing but [`MemoryStorage`] — the default on a toy OS and
//! in tests.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

/// Where a paint document is saved and loaded.
pub trait Storage {
    /// Writes `bytes`. An error is a short, human-readable message.
    fn save(&self, bytes: &[u8]) -> Result<(), String>;

    /// Reads the last saved bytes, or `None` when nothing is stored or the
    /// read failed.
    fn load(&self) -> Option<Vec<u8>>;

    /// Whether Save/Open should be offered.
    fn available(&self) -> bool;

    /// Writes `bytes` to a caller-chosen `path` (the Save As seam). The default
    /// is unsupported, so a path-less store never sees a path.
    fn save_to(&self, _path: &Path, _bytes: &[u8]) -> Result<(), String> {
        Err("unsupported".to_string())
    }

    /// Reads the bytes at a caller-chosen `path` (the Open seam), or `None` on
    /// any failure.
    fn load_from(&self, _path: &Path) -> Option<Vec<u8>> {
        None
    }

    /// Whether [`save_to`](Storage::save_to)/[`load_from`](Storage::load_from)
    /// work, i.e. whether file dialogs make sense for this store.
    fn supports_paths(&self) -> bool {
        false
    }

    /// The path [`save`](Storage::save)/[`load`](Storage::load) act on, if the
    /// store has one; a start-up load uses it as the document's path.
    fn default_path(&self) -> Option<PathBuf> {
        None
    }
}

/// An in-memory store: no file, no environment, always available. This is the
/// default, and the only storage a toy OS needs.
#[derive(Default)]
pub struct MemoryStorage {
    slot: RefCell<Option<Vec<u8>>>,
}

impl MemoryStorage {
    /// An empty in-memory store.
    pub fn new() -> MemoryStorage {
        MemoryStorage::default()
    }
}

impl Storage for MemoryStorage {
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        *self.slot.borrow_mut() = Some(bytes.to_vec());
        Ok(())
    }

    fn load(&self) -> Option<Vec<u8>> {
        self.slot.borrow().clone()
    }

    fn available(&self) -> bool {
        true
    }
}

/// A storage whose every operation fails; a test uses it to prove the app needs
/// no I/O.
pub struct FailingStorage;

impl Storage for FailingStorage {
    fn save(&self, _bytes: &[u8]) -> Result<(), String> {
        Err("storage unavailable".to_string())
    }

    fn load(&self) -> Option<Vec<u8>> {
        None
    }

    fn available(&self) -> bool {
        false
    }
}

/// A filesystem store, behind the off-by-default `fs` feature.
///
/// It writes only the exact path it was configured with. A path that is a
/// symlink (or whose parent is missing) makes it report itself unavailable, so
/// a write can never follow a link elsewhere. Tests never construct one.
#[cfg(feature = "fs")]
pub struct FsStorage {
    path: std::path::PathBuf,
}

#[cfg(feature = "fs")]
impl FsStorage {
    /// A store backed by `path`.
    pub fn new(path: impl Into<std::path::PathBuf>) -> FsStorage {
        FsStorage { path: path.into() }
    }

    /// Whether the path may be written: its parent exists and it is not an
    /// existing symlink.
    fn writable(&self) -> bool {
        match std::fs::symlink_metadata(&self.path) {
            Ok(meta) if meta.file_type().is_symlink() => false,
            Ok(_) => true,
            Err(_) => self
                .path
                .parent()
                .is_some_and(|parent| parent.as_os_str().is_empty() || parent.exists()),
        }
    }
}

#[cfg(feature = "fs")]
impl Storage for FsStorage {
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        if !self.writable() {
            return Err("refusing to write a symlink or missing directory".to_string());
        }
        std::fs::write(&self.path, bytes).map_err(|error| error.to_string())
    }

    fn load(&self) -> Option<Vec<u8>> {
        std::fs::read(&self.path).ok()
    }

    fn available(&self) -> bool {
        self.writable()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_storage_round_trips() {
        let storage = MemoryStorage::new();
        assert!(storage.available());
        assert_eq!(storage.load(), None);
        storage.save(b"hello").unwrap();
        assert_eq!(storage.load().as_deref(), Some(b"hello".as_slice()));
    }

    #[test]
    fn failing_storage_always_fails() {
        let storage = FailingStorage;
        assert!(!storage.available());
        assert!(storage.save(b"x").is_err());
        assert_eq!(storage.load(), None);
    }
}
