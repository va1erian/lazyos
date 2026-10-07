//! The LazyOS platform layer shared by the migrated apps: argument validation,
//! the pickers' default folder, the Paint storage and atomic writes, the Files
//! launcher, the clipboard client, the drag-and-drop `text/uri-list` codec,
//! the audio transport, the print spooler's interface, the central topics
//! broker's publisher and the Settings app's config store and system
//! services.
//!
//! Everything here is behind a small, testable seam so the apps stay portable
//! and the OS specifics live in one place.

pub mod accounts;
pub mod argv;
pub mod audio;
pub mod clipboard;
pub mod confd_store;
pub mod dirs;
pub mod elevd;
pub mod launcher;
pub mod messenger;
pub mod pkg;
pub mod print;
pub mod storage;
pub mod system;
pub mod topic_feed;
pub mod topics;
pub mod urilist;
