//! Saving pictures to disk (docs/lazysky-plan.md §7.6): one picture to a
//! path the user chose, and every picture of an account into a folder.
//!
//! The file is the bytes the server sent, never re-encoded. A write goes
//! through `atomicfile` (a temp file renamed when whole), so an interrupted
//! download never leaves a half picture under its real name.

mod bulk;
mod pool;
mod sanitize;

pub use bulk::{BulkState, Scan, Step};
pub use pool::{SaveJob, SavePool};
pub use sanitize::{extension_of, file_name_for, folder_for, sanitize};

