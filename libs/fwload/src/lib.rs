//! Device firmware files (docs/wifi-prerequisites-plan.md 3.8): a driver
//! names a file relative to the firmware tree ([`fhs::FIRMWARE`]), as Linux
//! does under `/lib/firmware` (`rtw88/rtw8822c_fw.bin`), and gets its bytes.
//!
//! The firmware is a separate file on the OS volume, never compiled into a
//! program: blobs come under their vendor's licence, not the project's
//! (`THIRD_PARTY.md`). The image build places them from the pinned downloads
//! of `tools/firmware/fetch.py`.
//!
//! What this crate decides, host-tested:
//!
//! * a name is `<vendor>/<file>` (more segments allowed, as linux-firmware
//!   has `ath11k/WCN6855/hw2.0/amss.bin`), each segment 1..=[`MAX_SEGMENT`]
//!   bytes of `[A-Za-z0-9._-]` not starting with `.`: no `..`, no absolute
//!   path, no empty segment, no backslash, so the composed path never leaves
//!   the tree (the tree is the image build's, and holds no symbolic links);
//! * the size is capped ([`DEFAULT_MAX_BYTES`] unless the driver asks for
//!   less) and checked before anything is allocated, so a damaged volume
//!   cannot make a driver allocate the disk;
//! * a file that changed size between the size check and the read is refused.
//!
//! The file system calls are the binary's ([`Reader`]); `wifid` implements it
//! over `user::files`.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// Longest name accepted, in bytes.
pub const MAX_NAME: usize = 192;
/// Longest segment accepted, in bytes.
pub const MAX_SEGMENT: usize = 64;
/// Most segments in a name (`vendor/chip/rev/file`).
pub const MAX_DEPTH: usize = 6;
/// The default size cap: above every Wi-Fi blob in linux-firmware today
/// (the RTL8822C's is 198 KiB, the largest ath11k images a few MiB), small
/// enough that a damaged file cannot exhaust a driver's heap.
pub const DEFAULT_MAX_BYTES: usize = 16 << 20;

/// Why a name was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameError {
    Empty,
    TooLong,
    /// Starts with `/`: names are relative to the firmware tree.
    Absolute,
    /// One segment only: a name is at least `<vendor>/<file>`.
    NoVendor,
    TooDeep,
    /// An empty segment (`a//b`, a trailing `/`).
    EmptySegment,
    /// A segment longer than [`MAX_SEGMENT`].
    LongSegment,
    /// A segment starting with `.` (`.`, `..`, a hidden file).
    Dot,
    /// A byte outside `[A-Za-z0-9._-]` (and `/` between segments).
    BadByte(u8),
}

/// Why a firmware file was not loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Name(NameError),
    /// The file is larger than the cap the driver gave.
    TooLarge { len: u64, max: usize },
    /// The file is empty.
    Empty,
    /// The file's size changed between the size check and the read.
    Changed { expected: u64, read: usize },
    /// The reader failed: a positive errno (`ENOENT` for a missing file).
    Io(i64),
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NameError::Empty => f.write_str("empty name"),
            NameError::TooLong => write!(f, "name longer than {MAX_NAME} bytes"),
            NameError::Absolute => f.write_str("absolute name"),
            NameError::NoVendor => f.write_str("no vendor directory"),
            NameError::TooDeep => write!(f, "more than {MAX_DEPTH} segments"),
            NameError::EmptySegment => f.write_str("empty segment"),
            NameError::LongSegment => write!(f, "segment longer than {MAX_SEGMENT} bytes"),
            NameError::Dot => f.write_str("segment starting with '.'"),
            NameError::BadByte(byte) => write!(f, "byte {byte:#04x} not allowed"),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Name(why) => write!(f, "bad name: {why}"),
            Error::TooLarge { len, max } => write!(f, "{len} bytes, over the {max}-byte cap"),
            Error::Empty => f.write_str("empty file"),
            Error::Changed { expected, read } => {
                write!(f, "size changed: {expected} bytes expected, {read} read")
            }
            Error::Io(errno) => write!(f, "read failed (errno {errno})"),
        }
    }
}

impl From<NameError> for Error {
    fn from(why: NameError) -> Error {
        Error::Name(why)
    }
}

/// Check a vendor-relative firmware name.
pub fn check_name(name: &str) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.len() > MAX_NAME {
        return Err(NameError::TooLong);
    }
    if name.starts_with('/') {
        return Err(NameError::Absolute);
    }
    let mut depth = 0;
    for segment in name.split('/') {
        depth += 1;
        check_segment(segment)?;
    }
    if depth < 2 {
        return Err(NameError::NoVendor);
    }
    if depth > MAX_DEPTH {
        return Err(NameError::TooDeep);
    }
    Ok(())
}

fn check_segment(segment: &str) -> Result<(), NameError> {
    if segment.is_empty() {
        return Err(NameError::EmptySegment);
    }
    if segment.len() > MAX_SEGMENT {
        return Err(NameError::LongSegment);
    }
    if segment.starts_with('.') {
        return Err(NameError::Dot);
    }
    match segment
        .bytes()
        .find(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')))
    {
        Some(byte) => Err(NameError::BadByte(byte)),
        None => Ok(()),
    }
}

/// The absolute path of firmware `name`: `fhs::FIRMWARE/<name>`.
pub fn path_of(name: &str) -> Result<String, NameError> {
    check_name(name)?;
    let mut path = String::with_capacity(fhs::FIRMWARE.len() + 1 + name.len());
    path.push_str(fhs::FIRMWARE);
    path.push('/');
    path.push_str(name);
    Ok(path)
}

/// The file system, as the binary reaches it. Errors are positive errnos.
pub trait Reader {
    /// The size in bytes of the regular file at `path` (a directory is an
    /// error).
    fn size(&mut self, path: &str) -> Result<u64, i64>;
    /// Read the whole file at `path` into `out` (cleared first); a reader
    /// that finds more than `max` bytes fails rather than read them.
    fn read(&mut self, path: &str, max: usize, out: &mut Vec<u8>) -> Result<(), i64>;
}

/// Load firmware `name`, at most `max` bytes (use [`DEFAULT_MAX_BYTES`]
/// unless the chip's own limit is smaller).
pub fn load<R: Reader>(reader: &mut R, name: &str, max: usize) -> Result<Vec<u8>, Error> {
    let path = path_of(name)?;
    let len = reader.size(&path).map_err(Error::Io)?;
    if len > max as u64 {
        return Err(Error::TooLarge { len, max });
    }
    if len == 0 {
        return Err(Error::Empty);
    }
    let mut bytes = Vec::new();
    reader.read(&path, max, &mut bytes).map_err(Error::Io)?;
    if bytes.len() as u64 != len || bytes.len() > max {
        return Err(Error::Changed {
            expected: len,
            read: bytes.len(),
        });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
