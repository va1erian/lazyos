//! The seam between the bindings and the operating system.
//!
//! Every effect a script can have (files, environment, clock, stdio) goes
//! through [`Host`], so the bindings are unit-tested against an in-memory mock
//! ([`crate::mock::MockHost`]) and a future native (`no_std`) host only has to
//! implement this trait. Fallible operations return a [`HostError`] carrying a
//! ready-to-show message; the bindings turn it into a catchable Rhai error.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// A failed host operation, already phrased for the user
/// (`"No such file or directory (os error 2)"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError(pub String);

impl HostError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a directory entry is, as reported to scripts (`kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

impl EntryKind {
    /// The string a script sees in `ls()[i].kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Dir => "dir",
            Self::Symlink => "symlink",
            Self::Other => "other",
        }
    }
}

/// One entry of [`Host::list_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub size: u64,
    pub kind: EntryKind,
}

/// Everything the `os` module needs from the surrounding process.
///
/// The `max` parameters are hard caps the implementation must enforce *while
/// reading* (not after), so a hostile file, a `/dev/zero`-style device or a
/// gigantic directory cannot exhaust memory before the size check runs.
pub trait Host {
    /// The script arguments (`rhai script.rhai a b` gives `["a", "b"]`).
    fn args(&self) -> Vec<String>;
    /// One environment variable; `None` when unset or not valid UTF-8.
    fn env_var(&self, key: &str) -> Option<String>;
    /// The whole environment (entries that are not valid UTF-8 are skipped).
    fn env_vars(&self) -> Vec<(String, String)>;
    /// Seconds on a monotonic clock (the epoch is unspecified).
    fn now_secs(&self) -> f64;
    /// Block for `ms` milliseconds.
    fn sleep_ms(&self, ms: u64);
    /// Read a whole file; an error if it is larger than `max` bytes.
    fn read_file(&self, path: &str, max: usize) -> Result<Vec<u8>, HostError>;
    /// Create or truncate `path` and write `data`.
    fn write_file(&self, path: &str, data: &[u8]) -> Result<(), HostError>;
    /// List a directory (any order; the bindings sort); an error if it has
    /// more than `max` entries.
    fn list_dir(&self, path: &str, max: usize) -> Result<Vec<DirEntry>, HostError>;
    /// Read all of standard input; an error if it is larger than `max` bytes.
    fn read_stdin(&self, max: usize) -> Result<Vec<u8>, HostError>;
    /// Write to standard output (`print`). An error means the reader went away.
    fn write_out(&self, text: &str) -> Result<(), HostError>;
    /// Write to standard error (`debug`). Best effort.
    fn write_err(&self, text: &str);
    /// The Messenger fabric, when this process can reach one. With `Some`,
    /// the engine gets the `msg` module; a host without a fabric (a plain
    /// Linux build, most tests) leaves it out.
    fn bus(&self) -> Option<Rc<dyn crate::msg::Bus>> {
        None
    }
}
