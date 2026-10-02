//! An in-memory [`BlockIo`] for host tests and the fuzz entry point.
//!
//! Clones share one image, so a test can drop a mounted volume and open the
//! same bytes again (a remount), or snapshot the bytes for the checker.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::vec::Vec;

use crate::{BlockIo, IoError, SECTOR_SIZE};

struct Shared {
    bytes: Mutex<Vec<u8>>,
    writable: AtomicBool,
    /// Sector writes still allowed; `u64::MAX` means no limit.
    write_budget: AtomicU64,
    flushes: AtomicU64,
    /// Write calls seen since `fail_write_number` armed a one-shot failure.
    write_calls: AtomicU64,
    /// The write call that fails once (`u64::MAX`: none armed).
    fail_call: AtomicU64,
    /// Requests and bytes, for the benchmarks: one vectored call is one request.
    reads: AtomicU64,
    read_bytes: AtomicU64,
    writes: AtomicU64,
    write_bytes: AtomicU64,
}

/// What a [`MemIo`] was asked to do (see [`MemIo::counters`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub reads: u64,
    pub read_bytes: u64,
    pub writes: u64,
    pub write_bytes: u64,
    pub flushes: u64,
}

/// A RAM disk of 512-byte sectors.
#[derive(Clone)]
pub struct MemIo {
    shared: Arc<Shared>,
}

impl MemIo {
    /// A zeroed disk of `bytes` (rounded up to whole sectors).
    pub fn new(bytes: usize) -> MemIo {
        MemIo::from_bytes(std::vec![0u8; bytes.div_ceil(SECTOR_SIZE) * SECTOR_SIZE])
    }

    /// A disk holding `bytes`, which must be a whole number of sectors.
    pub fn from_bytes(bytes: Vec<u8>) -> MemIo {
        assert!(bytes.len().is_multiple_of(SECTOR_SIZE), "partial sector");
        MemIo {
            shared: Arc::new(Shared {
                bytes: Mutex::new(bytes),
                writable: AtomicBool::new(true),
                write_budget: AtomicU64::new(u64::MAX),
                flushes: AtomicU64::new(0),
                write_calls: AtomicU64::new(0),
                fail_call: AtomicU64::new(u64::MAX),
                reads: AtomicU64::new(0),
                read_bytes: AtomicU64::new(0),
                writes: AtomicU64::new(0),
                write_bytes: AtomicU64::new(0),
            }),
        }
    }

    /// A copy of the current contents.
    pub fn snapshot(&self) -> Vec<u8> {
        self.shared.bytes.lock().unwrap().clone()
    }

    /// Run `f` over the live bytes (to corrupt them in place).
    pub fn with_bytes<R>(&self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        f(&mut self.shared.bytes.lock().unwrap())
    }

    /// Make the disk refuse (or accept) writes, like a read-only device.
    pub fn set_writable(&self, writable: bool) {
        self.shared.writable.store(writable, Ordering::Relaxed);
    }

    /// Allow only `sectors` more sector writes, then fail every one: a disk
    /// that dies part-way through an operation.
    pub fn fail_writes_after(&self, sectors: u64) {
        self.shared.write_budget.store(sectors, Ordering::Relaxed);
    }

    /// Fail exactly the `n`th write call from now (0 is the next one) and no
    /// other: a transient error part-way through an operation.
    pub fn fail_write_number(&self, n: u64) {
        self.shared.write_calls.store(0, Ordering::Relaxed);
        self.shared.fail_call.store(n, Ordering::Relaxed);
    }

    /// Write calls since `fail_write_number` last reset the count.
    pub fn write_calls(&self) -> u64 {
        self.shared.write_calls.load(Ordering::Relaxed)
    }

    /// Every request so far (all clones of this disk share the counts).
    pub fn counters(&self) -> Counters {
        let shared = &self.shared;
        Counters {
            reads: shared.reads.load(Ordering::Relaxed),
            read_bytes: shared.read_bytes.load(Ordering::Relaxed),
            writes: shared.writes.load(Ordering::Relaxed),
            write_bytes: shared.write_bytes.load(Ordering::Relaxed),
            flushes: shared.flushes.load(Ordering::Relaxed),
        }
    }

    /// How many times the volume asked the device to flush.
    pub fn flushes(&self) -> u64 {
        self.shared.flushes.load(Ordering::Relaxed)
    }

    fn span(&self, lba: u64, len: usize, bytes: usize) -> Result<core::ops::Range<usize>, IoError> {
        if !len.is_multiple_of(SECTOR_SIZE) {
            return Err(IoError::Failed);
        }
        let start = lba
            .checked_mul(SECTOR_SIZE as u64)
            .and_then(|start| usize::try_from(start).ok())
            .ok_or(IoError::Failed)?;
        let end = start.checked_add(len).ok_or(IoError::Failed)?;
        if end > bytes {
            return Err(IoError::Failed);
        }
        Ok(start..end)
    }
}

impl BlockIo for MemIo {
    fn sector_count(&self) -> u64 {
        (self.shared.bytes.lock().unwrap().len() / SECTOR_SIZE) as u64
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), IoError> {
        let bytes = self.shared.bytes.lock().unwrap();
        let range = self.span(lba, buf.len(), bytes.len())?;
        buf.copy_from_slice(&bytes[range]);
        self.shared.reads.fetch_add(1, Ordering::Relaxed);
        self.shared
            .read_bytes
            .fetch_add(buf.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), IoError> {
        if !self.is_writable() {
            return Err(IoError::ReadOnly);
        }
        let mut bytes = self.shared.bytes.lock().unwrap();
        let range = self.span(lba, buf.len(), bytes.len())?;
        let call = self.shared.write_calls.fetch_add(1, Ordering::Relaxed);
        if call == self.shared.fail_call.load(Ordering::Relaxed) {
            return Err(IoError::Failed);
        }
        let sectors = (buf.len() / SECTOR_SIZE) as u64;
        let budget = self.shared.write_budget.load(Ordering::Relaxed);
        if budget != u64::MAX {
            if budget < sectors {
                return Err(IoError::Failed);
            }
            self.shared
                .write_budget
                .store(budget - sectors, Ordering::Relaxed);
        }
        bytes[range].copy_from_slice(buf);
        self.shared.writes.fetch_add(1, Ordering::Relaxed);
        self.shared
            .write_bytes
            .fetch_add(buf.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    /// One request, like a real device's scatter list (the default would
    /// count one per buffer).
    fn read_sectors_vectored(&self, lba: u64, bufs: &mut [&mut [u8]]) -> Result<(), IoError> {
        let bytes = self.shared.bytes.lock().unwrap();
        let total = bufs.iter().map(|buf| buf.len()).sum();
        let mut at = self.span(lba, total, bytes.len())?.start;
        for buf in bufs.iter_mut() {
            buf.copy_from_slice(&bytes[at..at + buf.len()]);
            at += buf.len();
        }
        self.shared.reads.fetch_add(1, Ordering::Relaxed);
        self.shared
            .read_bytes
            .fetch_add(total as u64, Ordering::Relaxed);
        Ok(())
    }

    /// One request (one write call for the failure injection), all or nothing.
    fn write_sectors_vectored(&self, lba: u64, bufs: &[&[u8]]) -> Result<(), IoError> {
        let joined: Vec<u8> = bufs.concat();
        self.write_sectors(lba, &joined)
    }

    fn flush(&self) -> Result<(), IoError> {
        self.shared.flushes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn is_writable(&self) -> bool {
        self.shared.writable.load(Ordering::Relaxed)
    }
}
