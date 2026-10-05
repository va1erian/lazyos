#![forbid(unsafe_code)]

//! The platform seam: what LazyWriter needs from the system it runs on.
//!
//! The LazyOS binary fills it with the atomic writer from `xui_app`'s
//! platform layer, `$HOME` (or `/transient`) as the start folder and the
//! families it registered with the shaper; tests fill it with plain
//! `std::fs` and a temp folder.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_core::widget::{FileSystem, StdFileSystem};

/// Writes a whole file, replacing what was there.
pub type WriteFn = dyn Fn(&Path, &[u8]) -> Result<(), String>;

/// What the app takes from its platform.
#[derive(Clone)]
pub struct Host {
    /// Writes saved documents, exported Markdown and exported pictures. On
    /// LazyOS it writes a temp file and renames it, refusing symlinks.
    pub write: Rc<WriteFn>,
    /// Where the pickers open for an untitled document.
    pub start_dir: PathBuf,
    /// What the pickers browse.
    pub file_system: Rc<dyn FileSystem>,
    /// The family the Serif choice names, as the font file declares it.
    pub serif_family: String,
    /// The family the Mono choice names.
    pub mono_family: String,
    /// The printer the last successful job went to, for the print bar.
    pub last_printer: Rc<dyn Fn() -> Option<String>>,
    /// Remembers a printer that took a job.
    pub remember_printer: Rc<dyn Fn(&str)>,
}

impl Host {
    /// A host over `std::fs` with plain (non-atomic) writes and no memory of
    /// printers, for tests and host runs.
    pub fn std(start_dir: impl Into<PathBuf>) -> Host {
        Host {
            write: Rc::new(|path, bytes| std::fs::write(path, bytes).map_err(|e| e.to_string())),
            start_dir: start_dir.into(),
            file_system: Rc::new(StdFileSystem),
            serif_family: "serif".to_owned(),
            mono_family: "monospace".to_owned(),
            last_printer: Rc::new(|| None),
            remember_printer: Rc::new(|_| {}),
        }
    }
}
