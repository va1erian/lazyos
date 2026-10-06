//! The platform seam: what the viewer needs from the system it runs on. The
//! LazyOS binary fills it with `$HOME` for the Open dialog and the serial
//! console for evidence; tests use `std::fs`, a temp folder and a log they
//! read back.

use std::path::PathBuf;
use std::rc::Rc;

use xui_core::widget::{FileSystem, StdFileSystem};

/// Writes one line of evidence (the serial console on LazyOS).
pub type LogFn = dyn Fn(&str);

#[derive(Clone)]
pub struct Host {
    /// Where the Open dialog starts when no document is open.
    pub start_dir: PathBuf,
    /// What the Open dialog browses.
    pub file_system: Rc<dyn FileSystem>,
    /// Evidence lines (`PDF:...`).
    pub log: Rc<LogFn>,
    /// Render threads; `0` picks one per spare CPU.
    pub threads: usize,
}

impl Host {
    /// A host over `std::fs` that logs to stdout.
    pub fn std(start_dir: impl Into<PathBuf>) -> Host {
        Host {
            start_dir: start_dir.into(),
            file_system: Rc::new(StdFileSystem),
            log: Rc::new(|line| println!("{line}")),
            threads: 0,
        }
    }
}
