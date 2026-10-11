//! Saving pictures to disk (docs/lazysky-plan.md §7.6): one picture to a
//! path the user chose, and every picture of an account into a folder.
//!
//! The file is the bytes the server sent, never re-encoded. A write goes to
//! `<name>.part` and is renamed when whole, so an interrupted download never
//! leaves a half picture under its real name.

mod bulk;
mod pool;
mod sanitize;

pub use bulk::{BulkState, Scan, Step};
pub use pool::{SaveJob, SavePool};
pub use sanitize::{extension_of, file_name_for, folder_for, sanitize};

use std::path::{Path, PathBuf};

/// A saved file's temporary name.
pub fn part_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    path.with_file_name(name)
}
