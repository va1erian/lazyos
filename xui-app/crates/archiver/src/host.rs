//! The platform seam: what the Archiver needs from the system it runs on.
//!
//! The LazyOS binary fills it with `$HOME` as the pickers' start folder,
//! `/tmp` for "open inside" and drags, the `mimed` launcher and serial
//! logging; tests use plain `std::fs`, a temp folder and a recording
//! launcher.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_core::widget::{FileSystem, StdFileSystem};

/// Opens a file with the system's handler for its type.
pub type LaunchFn = dyn Fn(&Path) -> Result<(), String>;
/// Writes one line of evidence (the serial console on LazyOS).
pub type LogFn = dyn Fn(&str);

/// What the app takes from its platform.
#[derive(Clone)]
pub struct Host {
    /// Where the pickers open when no archive is open.
    pub start_dir: PathBuf,
    /// A private folder for files opened from inside an archive and for
    /// dragged-out entries.
    pub temp_dir: PathBuf,
    /// What the pickers browse.
    pub file_system: Rc<dyn FileSystem>,
    /// Opens an extracted file with its handler.
    pub launch: Rc<LaunchFn>,
    /// Evidence lines (`ARCHIVER:...`).
    pub log: Rc<LogFn>,
    /// Folders never offered as a place to extract to or create in (the
    /// read-only system tree on LazyOS); the start folder is offered instead.
    pub read_only_roots: Vec<PathBuf>,
}

impl Host {
    /// Whether `dir` is a sensible default destination: not below a
    /// read-only root, and not marked read-only.
    pub fn suggests(&self, dir: &Path) -> bool {
        !self
            .read_only_roots
            .iter()
            .any(|root| dir.starts_with(root))
            && std::fs::metadata(dir).is_ok_and(|meta| !meta.permissions().readonly())
    }

    /// A host over `std::fs` that launches nothing and logs to stdout.
    pub fn std(start_dir: impl Into<PathBuf>) -> Host {
        let start_dir = start_dir.into();
        Host {
            temp_dir: std::env::temp_dir().join(format!("archiver-{}", std::process::id())),
            start_dir,
            file_system: Rc::new(StdFileSystem),
            launch: Rc::new(|_| Err("no launcher".to_owned())),
            log: Rc::new(|line| println!("{line}")),
            read_only_roots: Vec::new(),
        }
    }
}
