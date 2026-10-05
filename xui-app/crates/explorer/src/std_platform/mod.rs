#![forbid(unsafe_code)]

//! The std-backed platform: the desktop filesystem seam, and the copy a
//! drop into a folder window runs.

pub mod copy;
mod fs;
mod launcher;

pub use copy::{CopyReport, copy_into};
pub use fs::StdPlatform;
pub use launcher::DesktopLauncher;
