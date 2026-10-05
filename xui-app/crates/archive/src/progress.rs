//! Progress reporting and cancellation, shared between a worker thread and
//! the UI that polls it.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result, CANCELLED};

/// What a running operation has done so far. Every field is safe to read from
/// another thread at any time.
#[derive(Debug, Default)]
pub struct Progress {
    total: AtomicU64,
    done: AtomicU64,
    items: AtomicU64,
    cancelled: AtomicBool,
    current: Mutex<String>,
}

/// A copy of a [`Progress`] at one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Bytes the operation expects to process (0 when unknown).
    pub total: u64,
    /// Bytes processed so far.
    pub done: u64,
    /// Entries finished so far.
    pub items: u64,
    /// The entry being processed.
    pub current: String,
}

impl Snapshot {
    /// Done as a fraction of the total in `0..=max`, or 0 when the total is
    /// unknown.
    pub fn scaled(&self, max: i32) -> i32 {
        if self.total == 0 {
            return 0;
        }
        let ratio = (self.done.min(self.total) as f64) / (self.total as f64);
        (ratio * f64::from(max)).round() as i32
    }
}

impl Progress {
    /// A fresh progress with nothing done.
    pub fn new() -> Progress {
        Progress::default()
    }

    /// Ask the operation to stop at its next read or write.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Whether [`cancel`](Self::cancel) was called.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Fail with [`Error::Cancelled`] once cancelled.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Set the bytes the operation expects to process.
    pub fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Relaxed);
    }

    /// Count `bytes` more as processed.
    pub fn add(&self, bytes: u64) {
        self.done.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Name the entry being processed and count the previous one finished.
    pub fn begin(&self, name: &str) {
        let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        if !current.is_empty() {
            self.items.fetch_add(1, Ordering::Relaxed);
        }
        current.clear();
        current.push_str(name);
    }

    /// The current state.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            total: self.total.load(Ordering::Relaxed),
            done: self.done.load(Ordering::Relaxed),
            items: self.items.load(Ordering::Relaxed),
            current: self
                .current
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }
}

/// A reader (or writer) that counts what passes through it into a
/// [`Progress`] and fails with a cancellation error once the progress is
/// cancelled. It owns a handle on the progress, so it can cross to a
/// decoder's helper thread.
pub struct Counted<R> {
    inner: R,
    progress: Arc<Progress>,
}

impl<R> Counted<R> {
    pub fn new(inner: R, progress: &Arc<Progress>) -> Counted<R> {
        Counted {
            inner,
            progress: Arc::clone(progress),
        }
    }
}

fn cancelled() -> io::Error {
    io::Error::other(CANCELLED)
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.progress.is_cancelled() {
            return Err(cancelled());
        }
        let n = self.inner.read(buf)?;
        self.progress.add(n as u64);
        Ok(n)
    }
}

impl<W: Write> Write for Counted<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.progress.is_cancelled() {
            return Err(cancelled());
        }
        let n = self.inner.write(buf)?;
        self.progress.add(n as u64);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_read_fails_as_cancelled() {
        let progress = Arc::new(Progress::new());
        let mut reader = Counted::new(&b"abc"[..], &progress);
        let mut buf = [0u8; 2];
        assert_eq!(reader.read(&mut buf).unwrap(), 2);
        progress.cancel();
        let error = reader.read(&mut buf).unwrap_err();
        assert!(Error::from(error).is_cancelled());
        assert_eq!(progress.snapshot().done, 2);
    }

    #[test]
    fn scaling_handles_an_unknown_total() {
        let mut snap = Snapshot::default();
        assert_eq!(snap.scaled(100), 0);
        snap.total = 200;
        snap.done = 50;
        assert_eq!(snap.scaled(100), 25);
        snap.done = 900;
        assert_eq!(snap.scaled(100), 100);
    }

    #[test]
    fn begin_counts_finished_items() {
        let progress = Progress::new();
        progress.begin("a");
        progress.begin("b");
        let snap = progress.snapshot();
        assert_eq!((snap.items, snap.current.as_str()), (1, "b"));
    }
}
