//! The host for tests and host runs: [`StdHost`] is over `std::fs`, launches
//! nothing, and keeps the evidence it is given with the channel prefix the
//! real host adds, so a test reads the same lines a session script does.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_core::widget::{FileSystem, StdFileSystem};

use crate::{AppHost, Report};

/// The evidence a host collected, shared so a test can read it after the app
/// took the host.
pub type Lines = Rc<RefCell<Vec<String>>>;

/// Writes a file: what [`StdHost::on_write`] installs.
pub type WriteFn = Rc<dyn Fn(&Path, &[u8]) -> Result<(), String>>;

/// A host over `std::fs` that writes plainly and keeps its evidence.
pub struct StdHost {
    start_dir: PathBuf,
    temp_dir: PathBuf,
    prefix: String,
    read_only: Vec<PathBuf>,
    lines: Lines,
    writer: Option<WriteFn>,
}

impl StdHost {
    /// A host whose pickers open in `start_dir`.
    pub fn new(start_dir: impl Into<PathBuf>) -> StdHost {
        StdHost {
            start_dir: start_dir.into(),
            temp_dir: std::env::temp_dir(),
            prefix: String::new(),
            read_only: Vec::new(),
            lines: Lines::default(),
            writer: None,
        }
    }

    /// Prefixes every recorded line with `channel:`, as the real host's
    /// `trace` does (`PDF` records `PDF:OPEN:PASS:/a.pdf`).
    pub fn channel(mut self, prefix: &str) -> StdHost {
        self.prefix = format!("{prefix}:");
        self
    }

    /// Keeps the evidence in `lines`, which the test already holds.
    pub fn collecting(mut self, lines: Lines) -> StdHost {
        self.lines = lines;
        self
    }

    /// The private folder [`AppHost::temp_dir`] reports.
    pub fn temp(mut self, dir: impl Into<PathBuf>) -> StdHost {
        self.temp_dir = dir.into();
        self
    }

    /// Folders [`AppHost::suggests`] refuses.
    pub fn read_only(mut self, roots: Vec<PathBuf>) -> StdHost {
        self.read_only = roots;
        self
    }

    /// What [`AppHost::write`] does (a plain `std::fs::write` by default).
    pub fn on_write(
        mut self,
        write: impl Fn(&Path, &[u8]) -> Result<(), String> + 'static,
    ) -> StdHost {
        self.writer = Some(Rc::new(write));
        self
    }

    /// The evidence so far.
    pub fn lines(&self) -> Vec<String> {
        self.lines.borrow().clone()
    }
}

impl Report for StdHost {
    fn report(&self, line: &str) {
        self.lines
            .borrow_mut()
            .push(format!("{}{line}", self.prefix));
    }
}

impl AppHost for StdHost {
    fn file_system(&self) -> Rc<dyn FileSystem> {
        Rc::new(StdFileSystem)
    }

    fn start_dir(&self) -> PathBuf {
        self.start_dir.clone()
    }

    fn temp_dir(&self) -> PathBuf {
        self.temp_dir.clone()
    }

    fn read_only_roots(&self) -> Vec<PathBuf> {
        self.read_only.clone()
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        match &self.writer {
            Some(write) => write(path, bytes),
            None => std::fs::write(path, bytes).map_err(|error| error.to_string()),
        }
    }
}
