//! Errors surfaced by the package reader.
//!
//! Every error is an `enum` with a one-line [`core::fmt::Display`], because the
//! installer shows these to a user. Manifest validation collects *every*
//! problem in one pass: [`ManifestError`] carries a [`Problem`] per mistake, so
//! one round trip can list everything wrong instead of only the first.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// One problem found while parsing or validating the manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    message: String,
}

impl Problem {
    pub(crate) fn new(message: String) -> Problem {
        Problem { message }
    }

    /// The one-line, user-facing description.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// Every manifest problem found in one pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestError {
    problems: Vec<Problem>,
}

impl ManifestError {
    pub(crate) fn new(problems: Vec<Problem>) -> ManifestError {
        ManifestError { problems }
    }

    /// The individual problems, in the order they were found.
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("manifest")?;
        for (index, problem) in self.problems.iter().enumerate() {
            formatter.write_str(if index == 0 { ": " } else { "; " })?;
            write!(formatter, "{problem}")?;
        }
        Ok(())
    }
}

/// Why [`crate::Package::open`] refused an archive.
///
/// The archive is parsed structurally before the manifest is touched, so a
/// malformed zip never allocates the manifest and a manifest problem always
/// reports as [`OpenError::Manifest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// No end-of-central-directory record in the last 64 KiB + 22 bytes.
    NoEndOfCentralDirectory,
    /// A zip64 EOCD, locator, or extra field is present; zip64 is not supported.
    Zip64Unsupported,
    /// The archive spans multiple disks (disk numbers or entry counts disagree).
    MultiDiskUnsupported,
    /// The central directory lies outside the archive or overlaps itself.
    BadCentralDirectory,
    /// A central directory record is truncated or has the wrong signature.
    BadCentralRecord,
    /// A local header is truncated, out of range, or disagrees with the central
    /// directory.
    BadLocalHeader { name: String },
    /// An entry uses a compression method other than stored or deflate.
    UnsupportedCompression { name: String, method: u16 },
    /// An entry is encrypted or relies on a data descriptor.
    UnsupportedFlags { name: String },
    /// The archive holds more than [`crate::MAX_ENTRIES`] entries.
    TooManyEntries { count: u64 },
    /// An entry claims more than [`crate::MAX_ENTRY_UNCOMPRESSED`] bytes.
    EntryTooLarge { name: String, size: u64 },
    /// The entries claim more than [`crate::MAX_TOTAL_UNCOMPRESSED`] bytes.
    TotalTooLarge { total: u64 },
    /// An entry name is empty, too long, not UTF-8, or holds a control byte.
    BadName { reason: &'static str },
    /// An entry name escapes the package or is otherwise unsafe.
    BadPath { name: String, reason: &'static str },
    /// Two entries share a name.
    DuplicateName { name: String },
    /// Two entries differ only in case (the target filesystem may fold case).
    CaseCollision { name: String },
    /// An entry sits outside the package layout.
    Layout { name: String, reason: &'static str },
    /// The package has no `manifest.toml`.
    NoManifest,
    /// The manifest is present but invalid; carries every problem found.
    Manifest(ManifestError),
    /// A required icon is missing, unreadable, or not a PNG.
    BadIcon { name: String },
}

impl From<ManifestError> for OpenError {
    fn from(error: ManifestError) -> OpenError {
        OpenError::Manifest(error)
    }
}

impl fmt::Display for OpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::NoEndOfCentralDirectory => {
                formatter.write_str("not a zip archive: no end-of-central-directory record")
            }
            OpenError::Zip64Unsupported => formatter.write_str("zip64 archives are not supported"),
            OpenError::MultiDiskUnsupported => {
                formatter.write_str("multi-disk zip archives are not supported")
            }
            OpenError::BadCentralDirectory => {
                formatter.write_str("the central directory is outside the archive")
            }
            OpenError::BadCentralRecord => {
                formatter.write_str("a central directory record is malformed")
            }
            OpenError::BadLocalHeader { name } => {
                write!(formatter, "entry {name:?} has an inconsistent local header")
            }
            OpenError::UnsupportedCompression { name, method } => write!(
                formatter,
                "entry {name:?} uses unsupported compression method {method}"
            ),
            OpenError::UnsupportedFlags { name } => {
                write!(
                    formatter,
                    "entry {name:?} is encrypted or uses a data descriptor"
                )
            }
            OpenError::TooManyEntries { count } => write!(
                formatter,
                "too many entries: {count} (maximum {})",
                crate::MAX_ENTRIES
            ),
            OpenError::EntryTooLarge { name, size } => write!(
                formatter,
                "entry {name:?} claims {size} bytes (maximum {})",
                crate::MAX_ENTRY_UNCOMPRESSED
            ),
            OpenError::TotalTooLarge { total } => write!(
                formatter,
                "the package expands to {total} bytes (maximum {})",
                crate::MAX_TOTAL_UNCOMPRESSED
            ),
            OpenError::BadName { reason } => write!(formatter, "invalid entry name: {reason}"),
            OpenError::BadPath { name, reason } => write!(formatter, "entry {name:?} {reason}"),
            OpenError::DuplicateName { name } => write!(formatter, "duplicate entry {name:?}"),
            OpenError::CaseCollision { name } => write!(
                formatter,
                "entry {name:?} differs only in case from another entry"
            ),
            OpenError::Layout { name, reason } => write!(formatter, "entry {name:?} {reason}"),
            OpenError::NoManifest => formatter.write_str("the package has no manifest.toml"),
            OpenError::Manifest(error) => write!(formatter, "{error}"),
            OpenError::BadIcon { name } => write!(
                formatter,
                "icon {name:?} is missing, unreadable, or not a PNG"
            ),
        }
    }
}

/// Why [`crate::Package::read`] refused an entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// No entry has that exact name.
    NoSuchEntry,
    /// The name is a directory, which has no data.
    IsDirectory,
    /// The compressed stream is malformed.
    Corrupt { name: String },
    /// The stream produced a different number of bytes than declared.
    SizeMismatch {
        name: String,
        expected: u32,
        actual: u32,
    },
    /// The extracted bytes have a different CRC-32 than the central directory.
    CrcMismatch {
        name: String,
        expected: u32,
        actual: u32,
    },
}

impl fmt::Display for ReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::NoSuchEntry => formatter.write_str("no such entry"),
            ReadError::IsDirectory => formatter.write_str("the entry is a directory"),
            ReadError::Corrupt { name } => write!(formatter, "entry {name:?} is corrupt"),
            ReadError::SizeMismatch {
                name,
                expected,
                actual,
            } => write!(
                formatter,
                "entry {name:?} expanded to {actual} bytes, expected {expected}"
            ),
            ReadError::CrcMismatch {
                name,
                expected,
                actual,
            } => write!(
                formatter,
                "entry {name:?} failed its CRC-32 check ({actual:#010x}, expected {expected:#010x})"
            ),
        }
    }
}

/// Why [`crate::Package::read_chunks`] stopped: the entry is bad, or the
/// sink refused a piece (and nothing more was produced).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChunkError<E> {
    Read(ReadError),
    Sink(E),
}
