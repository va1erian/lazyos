//! What the cache needs of a device: its faults, its requests and its
//! counters.

use fatfs::IoError;

/// Why a device request failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The device reported an error.
    Io,
    /// The medium is gone (unplugged): the volume is over.
    Gone,
    /// A write to a read-only medium.
    ReadOnly,
    /// A read or write ran past the end.
    Eof,
}

impl IoError for Fault {
    fn is_interrupted(&self) -> bool {
        false
    }

    fn new_unexpected_eof_error() -> Self {
        Fault::Eof
    }

    fn new_write_zero_error() -> Self {
        Fault::Io
    }
}

/// A raw device of 512-byte sectors.
pub trait BlockIo {
    /// Sectors on the device (the volume's window of the disk).
    fn sectors(&self) -> u64;
    /// Read `buf.len() / 512` sectors from `lba`; `buf` is a whole number of
    /// sectors, at most [`super::MAX_IO`].
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), Fault>;
    /// Write whole sectors from `lba`; at most [`super::MAX_IO`].
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), Fault>;
    /// Make everything written so far durable on the device.
    fn flush(&mut self) -> Result<(), Fault>;
}

/// Counters, for tests and the daemon's log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub reads: u64,
    pub writes: u64,
    pub hits: u64,
    pub misses: u64,
}
