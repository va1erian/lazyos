//! The library's error type: what went wrong, in words a status bar can show.

use std::fmt;
use std::io;

/// A failed archive operation.
#[derive(Debug)]
pub enum Error {
    /// The user cancelled through [`Progress::cancel`](crate::Progress::cancel).
    Cancelled,
    /// Reading or writing a file failed.
    Io(io::Error),
    /// The archive is damaged or not what it claims to be.
    Corrupt(String),
    /// The archive uses something this library does not implement (a
    /// compression method, encryption, a format it cannot write).
    Unsupported(String),
}

/// A `Result` with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// A [`Error::Corrupt`] with `message`.
    pub fn corrupt(message: impl Into<String>) -> Error {
        Error::Corrupt(message.into())
    }

    /// A [`Error::Unsupported`] with `message`.
    pub fn unsupported(message: impl Into<String>) -> Error {
        Error::Unsupported(message.into())
    }

    /// Whether this is a cancellation, not a failure.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Error::Cancelled)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Cancelled => f.write_str("cancelled"),
            Error::Io(error) => write!(f, "{error}"),
            Error::Corrupt(message) => write!(f, "damaged archive: {message}"),
            Error::Unsupported(message) => write!(f, "not supported: {message}"),
        }
    }
}

impl std::error::Error for Error {}

/// The message a cancelled [`Counted`](crate::progress::Counted) read fails
/// with, so the error can be told apart from a real I/O failure once it has
/// travelled through a decoder.
pub(crate) const CANCELLED: &str = "lazyarc: cancelled";

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Error {
        if error
            .get_ref()
            .is_some_and(|inner| inner.to_string() == CANCELLED)
        {
            return Error::Cancelled;
        }
        // Decoders report a damaged stream as `InvalidData`; say so in the
        // archive's terms rather than as an I/O failure.
        match error.kind() {
            io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => {
                Error::Corrupt(error.to_string())
            }
            io::ErrorKind::Unsupported => Error::Unsupported(error.to_string()),
            _ => Error::Io(error),
        }
    }
}

impl From<Error> for io::Error {
    fn from(error: Error) -> io::Error {
        match error {
            Error::Io(error) => error,
            Error::Cancelled => io::Error::other(CANCELLED),
            Error::Corrupt(message) => io::Error::new(io::ErrorKind::InvalidData, message),
            Error::Unsupported(message) => io::Error::new(io::ErrorKind::Unsupported, message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_read_survives_the_round_trip_through_io_error() {
        let error: io::Error = Error::Cancelled.into();
        assert!(Error::from(error).is_cancelled());
    }

    #[test]
    fn invalid_data_reads_as_a_damaged_archive() {
        let error = io::Error::new(io::ErrorKind::InvalidData, "bad block");
        assert!(matches!(Error::from(error), Error::Corrupt(_)));
    }
}
