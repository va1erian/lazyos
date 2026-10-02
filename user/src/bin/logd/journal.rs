//! `logd`'s persistent journals: [`logstore::Store`] bound to `/logs`
//! ([`fhs::state::LOGS_ROOT`]) through the native file syscalls.
//!
//! At start `logd` probes the directory (it exists on the OS volume, made by
//! the image build). If the probe fails the journals are absent for this
//! boot: `logd` keeps its ring, reports `degraded` and prints
//! `LOGD:STORE:ABSENT`. A write that fails later (a full disk) does the same
//! from then on (`LOGD:STORE:DEGRADED`); the ring keeps every record either
//! way, and nothing is retried, so a half-written line is never repeated.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use fhs::state::LOGS_ROOT;
use logstore::{JournalFs, Store, TailError};
use user::files::{self, Kind};
use user::sys;

use crate::ring::Record;

const ENOENT: i64 = 2;
const EISDIR: i64 = 21;
const EFBIG: i64 = 27;
/// Largest journal `TailFile` reads. `logd`'s own files stay under
/// [`logstore::FILE_CAP`]; a bigger one (`pkg.log`) is refused with `EFBIG`.
/// The buffer is allocated once: blocks this size are never reclaimed by the
/// user heap.
const READ_CAP: usize = logstore::FILE_CAP as usize + 16 * 1024;

/// [`JournalFs`] over `/logs`; errors are errnos.
pub(super) struct LogsDir;

fn path(name: &str) -> String {
    format!("{LOGS_ROOT}/{name}")
}

impl JournalFs for LogsDir {
    type Error = i64;

    fn list(&mut self) -> Result<Vec<(String, u64)>, i64> {
        Ok(files::list(LOGS_ROOT)?
            .into_iter()
            .filter(|entry| entry.kind == Kind::File)
            .map(|entry| (entry.name, entry.size))
            .collect())
    }

    fn append(&mut self, name: &str, data: &[u8]) -> Result<(), i64> {
        files::append_file(&path(name), data)
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), i64> {
        files::rename(&path(from), &path(to))
    }

    fn remove(&mut self, name: &str) -> Result<(), i64> {
        match files::remove(&path(name)) {
            Err(ENOENT) => Ok(()),
            other => other,
        }
    }

    fn sync(&mut self) -> Result<(), i64> {
        files::fsync(LOGS_ROOT)
    }

    fn read(&mut self, name: &str, out: &mut Vec<u8>) -> Result<bool, i64> {
        let path = path(name);
        let size = match files::stat(&path) {
            Ok((_, Kind::Dir)) => return Err(EISDIR),
            Ok((size, Kind::File)) => size as usize,
            Err(ENOENT) => return Ok(false),
            Err(errno) => return Err(errno),
        };
        if size > READ_CAP {
            return Err(EFBIG);
        }
        if out.capacity() < READ_CAP {
            out.reserve_exact(READ_CAP - out.len());
        }
        out.clear();
        out.resize(size, 0);
        let mut name_z = Vec::with_capacity(path.len() + 1);
        name_z.extend_from_slice(path.as_bytes());
        name_z.push(0);
        match sys::read_file(&name_z, out) {
            Some(read) => {
                out.truncate(read);
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

/// The store, or why there is none.
pub(super) struct Journals {
    store: Option<Store<LogsDir>>,
    /// Records persisted by a store that has since failed.
    persisted_before: u64,
    /// Why the journals are not written (`None` while they are).
    problem: Option<String>,
}

impl Journals {
    /// Probe `/logs` and open the store, printing the store marker.
    pub(super) fn open(now: u64) -> Journals {
        let boot = boot_id();
        let opened = probe().and_then(|()| {
            Store::open(LogsDir, boot, now)
                .map_err(|errno| format!("listing failed: {}", files::describe(errno)))
        });
        match opened {
            Ok(store) => {
                sys::write_str(&format!(
                    "LOGD:STORE:READY dir={LOGS_ROOT} boot={boot:016x} sources={} bytes={}\n",
                    store.ledger().len(),
                    store.ledger().total()
                ));
                Journals {
                    store: Some(store),
                    persisted_before: 0,
                    problem: None,
                }
            }
            Err(reason) => {
                sys::write_str(&format!(
                    "LOGD:STORE:ABSENT dir={LOGS_ROOT} reason=\"{reason}\"\n"
                ));
                Journals {
                    store: None,
                    persisted_before: 0,
                    problem: Some(reason),
                }
            }
        }
    }

    /// Buffer one ring record.
    pub(super) fn record(&mut self, record: &Record) {
        let Some(store) = &mut self.store else {
            return;
        };
        let result = store.append(record.seq, record.tick, &record.topic, &record.detail);
        self.check(result);
    }

    /// Flush on the timer.
    pub(super) fn tick(&mut self, now: u64) {
        if let Some(store) = &mut self.store {
            let result = store.tick(now);
            self.check(result);
        }
    }

    /// Flush and fsync (`Shutdown`).
    pub(super) fn sync(&mut self, now: u64) {
        if let Some(store) = &mut self.store {
            let result = store.sync(now);
            self.check(result);
        }
    }

    /// Records written to `/logs` this boot.
    pub(super) fn persisted(&self) -> u64 {
        self.persisted_before + self.store.as_ref().map_or(0, Store::persisted)
    }

    /// `healthd` status and detail.
    pub(super) fn health(&self) -> (&'static str, String) {
        match &self.problem {
            None => ("ok", format!("journals in {LOGS_ROOT}")),
            Some(reason) => ("degraded", format!("ring only: {reason}")),
        }
    }

    pub(super) fn sources(&mut self) -> Result<Vec<String>, i64> {
        match &mut self.store {
            Some(store) => store.sources(),
            None => Ok(Vec::new()),
        }
    }

    pub(super) fn tail(
        &mut self,
        source: &str,
        count: usize,
        max_bytes: usize,
    ) -> Result<Vec<String>, TailError<i64>> {
        match &mut self.store {
            Some(store) => {
                let result = store.tail(source, count, max_bytes);
                if let Err(TailError::Fs(errno)) = result {
                    // A refused read (EFBIG) is the caller's problem, not the
                    // store's; anything else stops the journals.
                    if errno != EFBIG {
                        self.check::<()>(Err(errno));
                    }
                }
                result
            }
            None if !logstore::valid_source(source) => Err(TailError::Invalid),
            None => Err(TailError::Missing),
        }
    }

    /// Stop using the store after a failed write.
    fn check<T>(&mut self, result: Result<T, i64>) {
        let Err(errno) = result else {
            return;
        };
        let Some(store) = self.store.take() else {
            return;
        };
        self.persisted_before += store.persisted();
        let reason = format!("write failed: {}", files::describe(errno));
        sys::write_str(&format!(
            "LOGD:STORE:DEGRADED reason=\"{reason}\" persisted={}\n",
            self.persisted_before
        ));
        self.problem = Some(reason);
    }
}

/// Whether `/logs` is a directory a file can be written to and removed from.
fn probe() -> Result<(), String> {
    match files::stat(LOGS_ROOT) {
        Ok((_, Kind::Dir)) => {}
        Ok(_) => return Err(String::from("not a directory")),
        Err(errno) => return Err(format!("stat failed: {}", files::describe(errno))),
    }
    let probe = path(".probe");
    files::write_file(&probe, b"ok")
        .map_err(|errno| format!("probe write failed: {}", files::describe(errno)))?;
    let _ = files::remove(&probe);
    Ok(())
}

/// This boot's id: kernel randomness, else the wall clock and the tick.
fn boot_id() -> u64 {
    let mut bytes = [0u8; 8];
    match sys::random(&mut bytes) {
        Ok(()) => u64::from_le_bytes(bytes),
        Err(_) => sys::wall_centis() ^ sys::clock().rotate_left(32),
    }
}
