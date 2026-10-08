//! Endpoints in an ordinary event loop (issue #667): the
//! [`op::ENDPOINT_FD`] op gives a Linux descriptor that `poll`, `select`
//! and `epoll` (and so `mio`, tokio, calloop) watch beside sockets.
//!
//! The descriptor reports readiness only (docs/architecture/endpoint-fd.md):
//! readable while a message is queued or the peer closed, writable while a
//! send would find room, `POLLHUP` once the peer closed, `POLLHUP | POLLERR`
//! once the handle it was made from is closed. Messages are still taken with
//! [`super::recv`]; `read`/`write` on the descriptor fail with `EINVAL`.

use super::{op, plain, MsgArgs};

/// A descriptor watching `handle`, close-on-exec when `cloexec`; the
/// descriptor number, or the negative errno (`-ENOENT` no such handle,
/// `-EINVAL` not a channel, `-EACCES` no `CALL` right, `-EMFILE`).
pub fn endpoint_fd(handle: u64, cloexec: bool) -> Result<i32, i64> {
    let args = MsgArgs {
        handle,
        flags: if cloexec { op::ENDPOINT_FD_CLOEXEC } else { 0 },
        ..MsgArgs::default()
    };
    let result = plain(op::ENDPOINT_FD, args)?;
    i32::try_from(result.value).map_err(|_| -crate::errno::EINVAL)
}

#[cfg(all(feature = "std", unix))]
mod std_fd {
    use std::io;
    use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

    use super::super::{AsRawHandle, OwnedHandle};

    /// An endpoint handle with the descriptor that watches it: register
    /// [`AsRawFd::as_raw_fd`] with `epoll` (or `mio::unix::SourceFd`), and
    /// `recv` on [`Pollable::handle`] when it reports readable.
    ///
    /// Dropping it closes the descriptor first, then releases the handle.
    #[derive(Debug)]
    pub struct Pollable {
        fd: OwnedFd,
        handle: OwnedHandle,
    }

    impl Pollable {
        /// Watch `handle` (taking ownership of it); the descriptor is
        /// close-on-exec.
        pub fn new(handle: OwnedHandle) -> io::Result<Pollable> {
            let raw =
                super::endpoint_fd(handle.as_raw_handle(), true).map_err(crate::errno::io_error)?;
            // SAFETY: the kernel just opened `raw` in this task's table for
            // this value alone; nothing else owns it.
            let fd = unsafe { OwnedFd::from_raw_fd(raw) };
            Ok(Pollable { fd, handle })
        }

        /// The watched endpoint handle.
        pub fn handle(&self) -> u64 {
            self.handle.as_raw_handle()
        }
    }

    impl AsRawHandle for Pollable {
        fn as_raw_handle(&self) -> u64 {
            self.handle()
        }
    }

    impl AsRawFd for Pollable {
        fn as_raw_fd(&self) -> RawFd {
            self.fd.as_raw_fd()
        }
    }

    impl AsFd for Pollable {
        fn as_fd(&self) -> BorrowedFd<'_> {
            self.fd.as_fd()
        }
    }
}

#[cfg(all(feature = "std", unix))]
pub use std_fd::Pollable;
