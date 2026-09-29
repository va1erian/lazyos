#![forbid(unsafe_code)]

//! The std-backed platform: the desktop filesystem seam.

mod fs;
mod launcher;

pub use fs::StdPlatform;
pub use launcher::DesktopLauncher;
