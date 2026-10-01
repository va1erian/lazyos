//! The `std` implementation of [`Host`]: real files, environment, clock and
//! stdio. Every read is bounded *while it happens* (`Take`), never after.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

use rhai_lazy::msg::Bus;
use rhai_lazy::{DirEntry, EntryKind, Host, HostError};

/// The running process as a script sees it.
pub struct StdHost {
    args: Vec<String>,
    started: Instant,
    bus: Option<Rc<dyn Bus>>,
}

impl StdHost {
    pub fn new(args: Vec<String>) -> Self {
        Self {
            args,
            started: Instant::now(),
            bus: fabric(),
        }
    }
}

/// The Messenger fabric when running on LazyOS (the `msg` module), else none.
#[cfg(target_arch = "x86_64")]
fn fabric() -> Option<Rc<dyn Bus>> {
    rhai_lazy::msg::gate::Gate::detect().map(|gate| Rc::new(gate) as Rc<dyn Bus>)
}

#[cfg(not(target_arch = "x86_64"))]
fn fabric() -> Option<Rc<dyn Bus>> {
    None
}

fn error(err: io::Error) -> HostError {
    HostError::new(err.to_string())
}

/// Read at most `max` bytes from `reader`; more than `max` is an error, but
/// only `max + 1` bytes are ever buffered.
pub fn read_bounded<R: Read>(reader: R, max: usize) -> Result<Vec<u8>, HostError> {
    let limit = (max as u64).saturating_add(1);
    let mut data = Vec::new();
    reader.take(limit).read_to_end(&mut data).map_err(error)?;
    if data.len() > max {
        return Err(HostError::new(format!("larger than the {max}-byte limit")));
    }
    Ok(data)
}

fn kind_of(file_type: fs::FileType) -> EntryKind {
    if file_type.is_dir() {
        EntryKind::Dir
    } else if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

impl Host for StdHost {
    fn args(&self) -> Vec<String> {
        self.args.clone()
    }

    fn env_var(&self, key: &str) -> Option<String> {
        // Odd keys (empty, `=`, NUL) can never be set: `var` reports them as
        // absent rather than panicking (pinned by a test).
        std::env::var(key).ok()
    }

    fn env_vars(&self) -> Vec<(String, String)> {
        std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect()
    }

    fn now_secs(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    fn sleep_ms(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn read_file(&self, path: &str, max: usize) -> Result<Vec<u8>, HostError> {
        let file = File::open(path).map_err(error)?;
        read_bounded(file, max)
    }

    fn write_file(&self, path: &str, data: &[u8]) -> Result<(), HostError> {
        fs::write(path, data).map_err(error)
    }

    fn list_dir(&self, path: &str, max: usize) -> Result<Vec<DirEntry>, HostError> {
        let mut entries = Vec::new();
        for item in fs::read_dir(path).map_err(error)? {
            if entries.len() >= max {
                return Err(HostError::new(format!("more than {max} entries")));
            }
            let item = item.map_err(error)?;
            let meta = item.metadata().map_err(error)?;
            entries.push(DirEntry {
                // Names that are not UTF-8 are shown with replacement
                // characters: the name is for display, not for reopening.
                name: item.file_name().to_string_lossy().into_owned(),
                size: meta.len(),
                kind: kind_of(meta.file_type()),
            });
        }
        Ok(entries)
    }

    fn read_stdin(&self, max: usize) -> Result<Vec<u8>, HostError> {
        read_bounded(io::stdin().lock(), max)
    }

    fn write_out(&self, text: &str) -> Result<(), HostError> {
        io::stdout()
            .lock()
            .write_all(text.as_bytes())
            .map_err(error)
    }

    fn write_err(&self, text: &str) {
        // Best effort: nothing useful can be done if stderr is gone.
        let _ = io::stderr().lock().write_all(text.as_bytes());
    }

    fn bus(&self) -> Option<Rc<dyn Bus>> {
        self.bus.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_reads_stop_at_the_limit() {
        assert_eq!(read_bounded(&b"abc"[..], 3).unwrap(), b"abc");
        assert!(read_bounded(&b"abcd"[..], 3).is_err());
        // An endless source is cut off, not read to the end.
        assert!(read_bounded(io::repeat(b'x'), 1000).is_err());
        assert_eq!(read_bounded(&b""[..], 0).unwrap(), b"");
    }

    #[test]
    fn file_round_trip_and_failures() {
        let dir = std::env::temp_dir().join(format!("rhai-host-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let host = StdHost::new(vec!["x".into()]);
        let file = dir.join("f.txt");
        let path = file.to_str().unwrap();
        host.write_file(path, b"hello").unwrap();
        assert_eq!(host.read_file(path, 100).unwrap(), b"hello");
        assert!(host.read_file(path, 2).is_err());
        assert!(host
            .read_file(dir.join("missing").to_str().unwrap(), 10)
            .is_err());
        let listing = host.list_dir(dir.to_str().unwrap(), 10).unwrap();
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].name, "f.txt");
        assert_eq!(listing[0].size, 5);
        assert_eq!(listing[0].kind, EntryKind::File);
        assert!(host.list_dir(dir.to_str().unwrap(), 0).is_err());
        assert!(host.list_dir(path, 10).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn env_lookups_never_panic_on_odd_keys() {
        let host = StdHost::new(Vec::new());
        assert_eq!(host.env_var(""), None);
        assert_eq!(host.env_var("A=B"), None);
        assert_eq!(host.env_var("NUL\0KEY"), None);
        assert_eq!(host.env_var("RHAI_HOST_SURELY_UNSET_VARIABLE"), None);
    }
}
