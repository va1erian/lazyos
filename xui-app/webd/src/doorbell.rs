//! The doorbell: how WebKit's thread wakes the service thread.
//!
//! The service thread parks in one `wait_any` on its registered endpoint (requests), the lifecycle
//! channel from `init` (Quit) and a descriptor. A thread that is not the service's cannot touch the
//! first two (a Messenger handle belongs to the task that made it), but it can write to a Linux
//! descriptor, and the wait names one (`WAIT_FD`). So the doorbell is an `eventfd`
//! (the same device `xui_app::backend::wakeup` rings for the UI loop):
//!
//! * the engine's callbacks ring it after they queue an event ([`ring`], through the queue's wake);
//! * the SIGTERM handler rings it ([`ring`] is a bare `write`, async-signal-safe);
//! * the service thread, woken, clears it ([`clear`]) **before** it drains the queue, so a ring that
//!   lands during the drain leaves the counter non-zero and the next park returns at once. No wake-up
//!   is lost and none is spent on an empty queue twice.
//!
//! The descriptor is a plain integer in a static so the signal handler needs no lock to reach it.

use std::os::fd::RawFd;
use std::sync::atomic::{AtomicI32, Ordering};

/// The eventfd, or -1 before [`create`].
static FD: AtomicI32 = AtomicI32::new(-1);

/// Make the doorbell. Once, before the engine can queue anything.
pub fn create() -> std::io::Result<()> {
    // SAFETY: a plain syscall with constant arguments.
    let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    FD.store(fd, Ordering::Release);
    Ok(())
}

/// The descriptor to park on with `WAIT_FD`, or `None` before [`create`].
pub fn fd() -> Option<RawFd> {
    let fd = FD.load(Ordering::Acquire);
    (fd >= 0).then_some(fd)
}

/// Ring: any thread, and a signal handler. A full counter (`EAGAIN`) means a wake-up is already
/// due, so a failed write loses nothing.
pub fn ring() {
    let fd = FD.load(Ordering::Acquire);
    if fd < 0 {
        return;
    }
    let one = 1u64.to_ne_bytes();
    // SAFETY: `fd` is the open eventfd and `one` is the eight bytes an eventfd write takes.
    let _ = unsafe { libc::write(fd, one.as_ptr().cast(), one.len()) };
}

/// Reset the counter to zero (a non-blocking read returns it and clears it). Service thread only.
pub fn clear() {
    let fd = FD.load(Ordering::Acquire);
    if fd < 0 {
        return;
    }
    let mut sink = [0u8; 8];
    // SAFETY: `fd` is the open eventfd and `sink` is the eight bytes a counter read returns.
    let _ = unsafe { libc::read(fd, sink.as_mut_ptr().cast(), sink.len()) };
}
