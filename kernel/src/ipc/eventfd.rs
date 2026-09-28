//! `eventfd(2)` objects (Linux ABI round 2).
//!
//! An eventfd is a counter plus a wait queue. A `read` takes the counter
//! (resetting it to zero, or decrementing it by one under `EFD_SEMAPHORE`); a
//! `write` adds to it, blocking while an addition would overflow the 64-bit
//! counter. `poll`/`epoll` report `POLLIN` while the counter is non-zero and
//! `POLLOUT` while a one-byte addition would still fit, matching Linux
//! `eventfd_poll`.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

use crate::ipc::pipe::{self, POLLIN, POLLOUT};
use crate::task::wait::WaitQueue;
use crate::task::{WaitKind, WakeReason};

/// An eventfd counter.
pub struct EventFd {
    counter: Mutex<u64>,
    /// Readers parked on a zero counter; writers parked on a saturated one.
    waiters: WaitQueue,
    nonblock: AtomicBool,
    /// `EFD_SEMAPHORE`: reads consume one instead of draining the counter.
    semaphore: bool,
    /// Writes so far, the freshness counter for edge-triggered `epoll`.
    writes: AtomicU64,
}

impl EventFd {
    /// Create a counter starting at `initial`.
    pub fn new(initial: u64, semaphore: bool) -> Arc<EventFd> {
        Arc::new(EventFd {
            counter: Mutex::new(initial),
            waiters: WaitQueue::new(WaitKind::Pipe),
            nonblock: AtomicBool::new(false),
            semaphore,
            writes: AtomicU64::new(0),
        })
    }

    /// `O_NONBLOCK` of the eventfd's open file description.
    pub fn nonblock(&self) -> bool {
        self.nonblock.load(Ordering::Acquire)
    }

    /// Set `O_NONBLOCK` (shared by `dup`/`fork`).
    pub fn set_nonblock(&self, on: bool) {
        self.nonblock.store(on, Ordering::Release);
    }

    /// The counter (diagnostics).
    pub fn value(&self) -> u64 {
        *self.counter.lock()
    }

    /// Read the eventfd's 8-byte value: zero the counter, or decrement it by
    /// one under `EFD_SEMAPHORE`. Blocks until the counter is non-zero unless
    /// non-blocking.
    pub fn read(&self) -> Result<u64, pipe::Error> {
        loop {
            {
                let mut counter = self.counter.lock();
                if *counter > 0 {
                    let value = if self.semaphore {
                        *counter -= 1;
                        1
                    } else {
                        core::mem::replace(&mut *counter, 0)
                    };
                    drop(counter);
                    self.waiters.notify_all(); // space for a blocked writer
                    crate::task::notify_poll();
                    return Ok(value);
                }
            }
            if self.nonblock() {
                return Err(pipe::Error::WouldBlock);
            }
            match self.waiters.wait(crate::task::current(), None) {
                WakeReason::Interrupted => return Err(pipe::Error::Interrupted),
                WakeReason::Woken | WakeReason::TimedOut => {}
            }
        }
    }

    /// Add `value` to the counter. `value == u64::MAX` is `Invalid`; an
    /// addition that would overflow blocks (or `WouldBlock`s) until a read
    /// makes room, like Linux.
    pub fn write(&self, value: u64) -> Result<(), pipe::Error> {
        if value == u64::MAX {
            return Err(pipe::Error::Invalid);
        }
        loop {
            {
                let mut counter = self.counter.lock();
                if let Some(next) = counter.checked_add(value) {
                    *counter = next;
                    drop(counter);
                    self.writes.fetch_add(1, Ordering::AcqRel);
                    self.waiters.notify_all();
                    crate::task::notify_poll();
                    return Ok(());
                }
            }
            if self.nonblock() {
                return Err(pipe::Error::WouldBlock);
            }
            match self.waiters.wait(crate::task::current(), None) {
                WakeReason::Interrupted => return Err(pipe::Error::Interrupted),
                WakeReason::Woken | WakeReason::TimedOut => {}
            }
        }
    }

    /// `poll` revents for the eventfd.
    pub fn poll(&self, events: u16) -> u16 {
        let counter = *self.counter.lock();
        let mut revents = 0;
        if events & POLLIN != 0 && counter > 0 {
            revents |= POLLIN;
        }
        if events & POLLOUT != 0 && counter < u64::MAX {
            revents |= POLLOUT;
        }
        revents
    }

    /// [`poll`](EventFd::poll) plus the freshness counter for edge-triggered
    /// `epoll` interests (every successful write).
    pub fn poll_gen(&self, events: u16) -> (u16, u64) {
        (self.poll(events), self.writes.load(Ordering::Acquire))
    }
}
