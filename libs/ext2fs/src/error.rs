//! The library's error type and the block-device seam it reads and writes through.

use alloc::vec::Vec;

/// Why an ext2 operation failed. The kernel maps each variant onto its own
/// error space in one place; the names follow the POSIX errors they stand for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ext2Error {
    NotFound,
    Exists,
    NotDir,
    IsDir,
    NotEmpty,
    ReadOnly,
    /// A malformed argument, or an image that contradicts itself.
    Invalid,
    NoSpace,
    NameTooLong,
    /// A valid image using something this driver deliberately does not do.
    NotSupported,
    /// The block device failed (not a read-only refusal).
    Io,
}

/// Why a [`BlockIo`] transfer failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IoError {
    /// A write to a device that cannot be written.
    ReadOnly,
    /// Anything else: out of range, a timeout, a bad status.
    Failed,
}

/// Bytes per device sector. Every transfer is a whole number of these; a host
/// whose device uses another sector size must refuse it before opening.
pub const SECTOR_SIZE: usize = 512;

/// A random-access device of 512-byte sectors. Implementations serialise
/// themselves (the driver may call from several tasks, one at a time per
/// volume), so every method takes `&self`.
pub trait BlockIo: Send + Sync {
    /// Number of sectors on the device.
    fn sector_count(&self) -> u64;

    /// Read whole sectors starting at `lba` into `buf` (a multiple of 512 bytes).
    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), IoError>;

    /// Write whole sectors starting at `lba` from `buf`.
    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), IoError>;

    /// Make every earlier write durable.
    fn flush(&self) -> Result<(), IoError>;

    /// Whether [`BlockIo::write_sectors`] can succeed. A volume on a device
    /// that cannot be written mounts read-only.
    fn is_writable(&self) -> bool;
}

/// A zero-filled `Vec` of exactly `len` bytes, or [`Ext2Error::NoSpace`]. File
/// sizes are caller-influenced and a small heap must refuse an oversized
/// request rather than abort.
pub fn zeroed(len: u64) -> Result<Vec<u8>, Ext2Error> {
    let len = usize::try_from(len).map_err(|_| Ext2Error::NoSpace)?;
    let mut data = Vec::new();
    data.try_reserve_exact(len)
        .map_err(|_| Ext2Error::NoSpace)?;
    data.resize(len, 0);
    Ok(data)
}
