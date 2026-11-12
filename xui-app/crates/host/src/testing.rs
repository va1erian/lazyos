//! Hosts for tests and host runs: one over `std::fs` ([`StdHost`]) and one
//! over memory ([`MemHost`]). Both keep the evidence they are given, with the
//! channel prefix the real host adds, so a test reads the same lines a
//! session script does.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_core::widget::{Entry, FileSystem, StdFileSystem};

use crate::{AppHost, Report};

/// The evidence a host collected, shared so a test can read it after the app
/// took the host.
pub type Lines = Rc<RefCell<Vec<String>>>;

/// Opens a file: what [`StdHost::on_launch`] installs.
pub type LaunchFn = Rc<dyn Fn(&Path) -> Result<(), String>>;
/// Writes a file: what [`StdHost::on_write`] installs.
pub type WriteFn = Rc<dyn Fn(&Path, &[u8]) -> Result<(), String>>;

/// A host over `std::fs` that launches nothing, writes plainly and keeps its
/// evidence.
pub struct StdHost {
    start_dir: PathBuf,
    temp_dir: PathBuf,
    prefix: String,
    read_only: Vec<PathBuf>,
    lines: Lines,
    launcher: Option<LaunchFn>,
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
            launcher: None,
            writer: None,
        }
    }

    /// Prefixes every recorded line with `channel:`, as the real host's
    /// `trace` does (`PDF` records `PDF:OPEN:PASS:/a.pdf`).
    pub fn channel(mut self, prefix: &str) -> StdHost {
        self.prefix = format!("{prefix}:");
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

    /// What [`AppHost::launch`] does (it refuses by default).
    pub fn on_launch(mut self, launch: impl Fn(&Path) -> Result<(), String> + 'static) -> StdHost {
        self.launcher = Some(Rc::new(launch));
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

    /// A handle on the evidence that outlives the host.
    pub fn shared_lines(&self) -> Lines {
        Rc::clone(&self.lines)
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

    fn launch(&self, path: &Path) -> Result<(), String> {
        match &self.launcher {
            Some(launch) => launch(path),
            None => Err("no launcher".to_owned()),
        }
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

/// A file system in memory: files by absolute path, directories implied by
/// them. The pickers browse it; [`MemHost`] writes into it.
#[derive(Default)]
pub struct MemFs {
    files: RefCell<BTreeMap<PathBuf, Vec<u8>>>,
}

impl MemFs {
    /// Stores `bytes` at `path`.
    pub fn put(&self, path: impl Into<PathBuf>, bytes: &[u8]) {
        self.files.borrow_mut().insert(path.into(), bytes.to_vec());
    }

    /// The bytes at `path`.
    pub fn read(&self, path: &Path) -> Option<Vec<u8>> {
        self.files.borrow().get(path).cloned()
    }
}

impl FileSystem for MemFs {
    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        let mut seen: BTreeMap<OsString, Entry> = BTreeMap::new();
        for (path, bytes) in self.files.borrow().iter() {
            let Ok(rest) = path.strip_prefix(dir) else {
                continue;
            };
            let mut parts = rest.components();
            let Some(first) = parts.next() else {
                continue;
            };
            let name = first.as_os_str().to_owned();
            let is_dir = parts.next().is_some();
            seen.entry(name.clone()).or_insert(Entry {
                name,
                is_dir,
                size: (!is_dir).then_some(bytes.len() as u64),
                modified: None,
            });
        }
        if seen.is_empty() && !self.is_dir(dir) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no such folder"));
        }
        Ok(seen.into_values().collect())
    }

    fn is_dir(&self, path: &Path) -> bool {
        path == Path::new("/")
            || self
                .files
                .borrow()
                .keys()
                .any(|file| file.starts_with(path) && file != path)
    }

    fn exists(&self, path: &Path) -> bool {
        self.files.borrow().contains_key(path) || self.is_dir(path)
    }

    fn home(&self) -> Option<PathBuf> {
        None
    }

    fn roots(&self) -> Vec<PathBuf> {
        vec![PathBuf::from("/")]
    }
}

/// A host with no disk at all: the file system is a [`MemFs`], writes land in
/// it, launches are recorded.
pub struct MemHost {
    /// What the pickers browse and writes change.
    pub fs: Rc<MemFs>,
    start_dir: PathBuf,
    prefix: String,
    lines: Lines,
    launched: RefCell<Vec<PathBuf>>,
}

impl MemHost {
    /// An empty file system, with the pickers starting in `start_dir`.
    pub fn new(start_dir: impl Into<PathBuf>) -> MemHost {
        MemHost {
            fs: Rc::default(),
            start_dir: start_dir.into(),
            prefix: String::new(),
            lines: Lines::default(),
            launched: RefCell::default(),
        }
    }

    /// Prefixes recorded lines with `channel:` (see [`StdHost::channel`]).
    pub fn channel(mut self, prefix: &str) -> MemHost {
        self.prefix = format!("{prefix}:");
        self
    }

    /// The evidence so far.
    pub fn lines(&self) -> Vec<String> {
        self.lines.borrow().clone()
    }

    /// The files [`AppHost::launch`] was asked to open.
    pub fn launched(&self) -> Vec<PathBuf> {
        self.launched.borrow().clone()
    }
}

impl Report for MemHost {
    fn report(&self, line: &str) {
        self.lines
            .borrow_mut()
            .push(format!("{}{line}", self.prefix));
    }
}

impl AppHost for MemHost {
    fn file_system(&self) -> Rc<dyn FileSystem> {
        Rc::clone(&self.fs) as Rc<dyn FileSystem>
    }

    fn start_dir(&self) -> PathBuf {
        self.start_dir.clone()
    }

    fn launch(&self, path: &Path) -> Result<(), String> {
        self.launched.borrow_mut().push(path.to_path_buf());
        Ok(())
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        self.fs.put(path, bytes);
        Ok(())
    }

    fn suggests(&self, dir: &Path) -> bool {
        self.fs.is_dir(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memory_host_writes_and_lists_through_the_same_files() {
        let host = MemHost::new("/home/user").channel("PDF");
        host.write(Path::new("/home/user/a/b.txt"), b"hi").unwrap();
        let fs = host.file_system();
        assert!(fs.is_dir(Path::new("/home/user/a")));
        let names: Vec<_> = fs
            .list(Path::new("/home/user"))
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.is_dir))
            .collect();
        assert_eq!(names, vec![(OsString::from("a"), true)]);
        assert_eq!(
            host.fs.read(Path::new("/home/user/a/b.txt")).unwrap(),
            b"hi"
        );
        assert!(fs.list(Path::new("/nowhere")).is_err());
    }

    #[test]
    fn evidence_carries_the_channel_prefix() {
        let host = StdHost::new("/").channel("WRITER");
        host.report("SAVE:PASS:/x");
        crate::report!(host, "OPEN:{}:{}", "PASS", 3);
        assert_eq!(host.lines(), ["WRITER:SAVE:PASS:/x", "WRITER:OPEN:PASS:3"]);
    }

    #[test]
    fn a_std_host_refuses_to_launch_unless_told_how() {
        assert!(StdHost::new("/").launch(Path::new("/a")).is_err());
        let host = StdHost::new("/").on_launch(|_| Ok(()));
        assert!(host.launch(Path::new("/a")).is_ok());
    }

    #[test]
    fn a_read_only_root_is_not_suggested() {
        let host = StdHost::new("/").read_only(vec![PathBuf::from("/system")]);
        assert!(!host.suggests(Path::new("/system/etc")));
    }
}
