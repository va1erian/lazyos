#![forbid(unsafe_code)]

//! [`DocumentError`]: why a load or save failed.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Why a document could not be loaded or saved.
#[derive(Debug)]
pub enum DocumentError {
    /// The file could not be read or written.
    Io {
        /// The path the operation used.
        path: PathBuf,
        /// The operating-system error.
        source: io::Error,
    },
    /// The file is not valid UTF-8. `offset` is the byte index of the first
    /// invalid byte.
    InvalidUtf8 {
        /// The path that was read.
        path: PathBuf,
        /// The byte index of the first invalid byte.
        offset: usize,
    },
    /// The file is larger than the cap.
    TooLarge {
        /// The path that was read.
        path: PathBuf,
        /// The file's size in bytes.
        size: u64,
        /// The cap that was exceeded.
        limit: u64,
    },
    /// The path names something that is not a regular file (a directory, say).
    NotAFile {
        /// The offending path.
        path: PathBuf,
    },
    /// A save was attempted on a document with no path; use
    /// [`Document::save_as`](super::Document::save_as) instead.
    NoPath,
}

impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocumentError::Io { path, source } => {
                write!(f, "{}: {source}", path.display())
            }
            DocumentError::InvalidUtf8 { path, offset } => write!(
                f,
                "{}: not valid UTF-8 (first bad byte at offset {offset})",
                path.display()
            ),
            DocumentError::TooLarge { path, size, limit } => write!(
                f,
                "{}: {size} bytes is over the {limit}-byte limit",
                path.display()
            ),
            DocumentError::NotAFile { path } => {
                write!(f, "{}: not a regular file", path.display())
            }
            DocumentError::NoPath => f.write_str("the document has no path; use Save As"),
        }
    }
}

impl std::error::Error for DocumentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DocumentError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
