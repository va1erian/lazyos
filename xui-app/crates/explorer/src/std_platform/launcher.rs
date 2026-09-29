#![forbid(unsafe_code)]

//! [`DesktopLauncher`]: hands a file to the OS opener with
//! [`Command`](std::process::Command) arguments — never a shell string.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::platform::Launcher;

/// The operating system's default file handler.
///
/// * Windows: `explorer.exe <path>`
/// * macOS: `open <path>`
/// * other Unix: `xdg-open <path>`
#[derive(Clone, Copy, Debug, Default)]
pub struct DesktopLauncher;

impl DesktopLauncher {
    /// The launcher.
    pub const fn new() -> DesktopLauncher {
        DesktopLauncher
    }
}

impl Launcher for DesktopLauncher {
    fn open(&self, path: &Path) -> io::Result<()> {
        let program = opener();
        // Detach: the handler keeps running after the explorer moves on. The
        // path is one argument, so a name with spaces or shell characters is
        // never interpreted.
        Command::new(program)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(())
    }
}

/// The per-OS opener program.
#[cfg(target_os = "windows")]
fn opener() -> &'static str {
    "explorer.exe"
}

#[cfg(target_os = "macos")]
fn opener() -> &'static str {
    "open"
}

#[cfg(all(unix, not(target_os = "macos")))]
fn opener() -> &'static str {
    "xdg-open"
}

#[cfg(not(any(unix, target_os = "windows")))]
fn opener() -> &'static str {
    "xdg-open"
}
