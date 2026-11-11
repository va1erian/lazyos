//! The errno values native syscalls return, as positive numbers (a syscall
//! returns `-EPERM`): the one table, defined in `libmessenger::errno` (the
//! crate with no dependencies, which `lazyos-sys` builds on) and re-exported
//! here under the path every program already uses.

pub use libmessenger::errno::*;

/// A negative errno as a [`std::io::Error`] (its `raw_os_error` is the
/// positive value, so `kind()` follows the Linux mapping).
#[cfg(feature = "std")]
pub fn io_error(code: i64) -> std::io::Error {
    let errno = i32::try_from(code.unsigned_abs()).unwrap_or(i32::MAX);
    std::io::Error::from_raw_os_error(errno)
}

// Linux numbering: the kind mapping only holds where the host is Linux too.
#[cfg(all(test, feature = "std", target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn a_negative_errno_maps_to_its_io_kind() {
        assert_eq!(io_error(-ENOENT).kind(), std::io::ErrorKind::NotFound);
        assert_eq!(io_error(-EACCES).raw_os_error(), Some(13));
        assert_eq!(io_error(-ETIMEDOUT).kind(), std::io::ErrorKind::TimedOut);
    }
}
