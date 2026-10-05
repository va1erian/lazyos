#![forbid(unsafe_code)]

//! The std-backed platform: the desktop filesystem seam, and the copy or
//! move a drop into a folder window runs.

pub mod copy;
mod fs;
mod launcher;
pub mod transfer;

pub use copy::{CopyReport, copy_into};
pub use fs::StdPlatform;
pub use launcher::DesktopLauncher;
pub use transfer::{DropReport, Intent, Transfer, drop_into};
