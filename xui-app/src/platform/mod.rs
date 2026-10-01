//! The LazyOS platform layer shared by the migrated apps: argument validation,
//! the Paint storage, the Files launcher and the clipboard client.
//!
//! Everything here is behind a small, testable seam so the apps stay portable
//! and the OS specifics live in one place.

pub mod argv;
pub mod clipboard;
pub mod confd_store;
pub mod dialog_fs;
pub mod files_fs;
pub mod launcher;
pub mod messenger;
pub mod pkg;
pub mod storage;
