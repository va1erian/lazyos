//! The wake-up on a host that is not Linux: none.
//!
//! The real one (`wakeup.rs`) is an `epoll` set around an `eventfd`, which
//! only a LazyOS (Linux ABI) process has. A Windows or macOS host builds this
//! crate for tools that never run a backend (`lzpack` packages a LazyRAD
//! project), so `new` fails and the loop falls back to its plain wait, as
//! it does when the real one cannot create its descriptors.

use std::io;

use xui_core::backend::Waker;

/// A descriptor number, as `std::os::fd::RawFd` on Unix.
pub(super) type RawFd = i32;

/// Never constructed: [`Wakeup::new`] always fails.
pub(super) struct Wakeup;

impl Wakeup {
    pub(super) fn new() -> io::Result<Wakeup> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "no epoll on this host"))
    }

    pub(super) fn waker(&self) -> Waker {
        Box::new(|| {})
    }

    pub(super) fn epoll_fd(&self) -> RawFd {
        -1
    }

    pub(super) fn forget(&self) {}

    pub(super) fn watch(&self, _fd: Option<RawFd>) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn collect(&self) -> bool {
        false
    }
}
