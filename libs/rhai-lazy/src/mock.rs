//! An in-memory [`Host`] for the host-side tests: a fake filesystem, environment,
//! stdin and captured stdout/stderr, with switches for the failure paths.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use crate::host::{DirEntry, EntryKind, Host, HostError};

/// The fake process. Interior mutability because [`Host`] takes `&self`.
#[derive(Default)]
pub struct MockHost {
    pub args: Vec<String>,
    pub env: RefCell<BTreeMap<String, String>>,
    pub files: RefCell<BTreeMap<String, Vec<u8>>>,
    pub dirs: RefCell<BTreeMap<String, Vec<DirEntry>>>,
    pub stdin: RefCell<Vec<u8>>,
    pub stdout: RefCell<String>,
    pub stderr: RefCell<String>,
    pub slept: RefCell<Vec<u64>>,
    /// Paths whose access is denied (`read_file`, `write_file`, `list_dir`).
    pub denied: RefCell<Vec<String>>,
    /// Every `write_out` fails, as when the reader of a pipe went away.
    pub stdout_closed: Cell<bool>,
    pub clock: Cell<f64>,
}

impl MockHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_file(self, path: &str, data: &[u8]) -> Self {
        self.files
            .borrow_mut()
            .insert(path.to_string(), data.to_vec());
        self
    }

    pub fn with_stdin(self, data: &[u8]) -> Self {
        *self.stdin.borrow_mut() = data.to_vec();
        self
    }

    pub fn with_env(self, key: &str, value: &str) -> Self {
        self.env
            .borrow_mut()
            .insert(key.to_string(), value.to_string());
        self
    }

    pub fn deny(&self, path: &str) {
        self.denied.borrow_mut().push(path.to_string());
    }

    pub fn out(&self) -> String {
        self.stdout.borrow().clone()
    }

    fn check_denied(&self, path: &str) -> Result<(), HostError> {
        if self.denied.borrow().iter().any(|d| d == path) {
            Err(HostError::new("Permission denied (os error 13)"))
        } else {
            Ok(())
        }
    }
}

impl Host for MockHost {
    fn args(&self) -> Vec<String> {
        self.args.clone()
    }

    fn env_var(&self, key: &str) -> Option<String> {
        self.env.borrow().get(key).cloned()
    }

    fn env_vars(&self) -> Vec<(String, String)> {
        self.env
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    fn now_secs(&self) -> f64 {
        self.clock.get()
    }

    fn sleep_ms(&self, ms: u64) {
        self.slept.borrow_mut().push(ms);
    }

    fn read_file(&self, path: &str, max: usize) -> Result<Vec<u8>, HostError> {
        self.check_denied(path)?;
        let files = self.files.borrow();
        let data = files
            .get(path)
            .ok_or_else(|| HostError::new("No such file or directory (os error 2)"))?;
        if data.len() > max {
            return Err(HostError::new("file is larger than the read limit"));
        }
        Ok(data.clone())
    }

    fn write_file(&self, path: &str, data: &[u8]) -> Result<(), HostError> {
        self.check_denied(path)?;
        self.files
            .borrow_mut()
            .insert(path.to_string(), data.to_vec());
        Ok(())
    }

    fn list_dir(&self, path: &str, max: usize) -> Result<Vec<DirEntry>, HostError> {
        self.check_denied(path)?;
        let dirs = self.dirs.borrow();
        let entries = dirs
            .get(path)
            .ok_or_else(|| HostError::new("No such file or directory (os error 2)"))?;
        if entries.len() > max {
            return Err(HostError::new("directory has too many entries"));
        }
        Ok(entries.clone())
    }

    fn read_stdin(&self, max: usize) -> Result<Vec<u8>, HostError> {
        let mut stdin = self.stdin.borrow_mut();
        if stdin.len() > max {
            return Err(HostError::new("input is larger than the read limit"));
        }
        Ok(core::mem::take(&mut *stdin))
    }

    fn write_out(&self, text: &str) -> Result<(), HostError> {
        if self.stdout_closed.get() {
            return Err(HostError::new("Broken pipe (os error 32)"));
        }
        self.stdout.borrow_mut().push_str(text);
        Ok(())
    }

    fn write_err(&self, text: &str) {
        self.stderr.borrow_mut().push_str(text);
    }
}

/// A directory entry for the fake filesystem.
pub fn entry(name: &str, size: u64, kind: EntryKind) -> DirEntry {
    DirEntry {
        name: name.to_string(),
        size,
        kind,
    }
}
