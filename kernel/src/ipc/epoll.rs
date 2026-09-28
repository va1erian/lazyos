//! `epoll(7)` objects (Linux ABI round 2).
//!
//! An epoll fd owns a list of interests: `(descriptor number, open file
//! handle, requested events, user data)`. `epoll_wait` scans the interest list
//! against the handles' own `poll` state; because the handle is cloned in (as
//! Linux holds a file reference), a scan never needs the task's descriptor
//! table and cannot race a `close`.
//!
//! Level-triggered interests report whenever the handle is ready. With
//! `EPOLLET`, each handle's [`poll_gen`](crate::ipc::pipe::Pipe::poll_gen)
//! freshness counter is remembered so a fresh edge (new data, new space, or a
//! close) is reported even while the handle stays ready; a repeated wait with
//! no change reports nothing. Data arrival on a still-ready pipe therefore
//! wakes an edge waiter, unlike a bare "was ready last time" snapshot.
//!
//! `close` of a user descriptor removes its interest (Linux drops the item
//! when the file is released); the cloned handle keeps the pipe/socket alive
//! until then. Interests are keyed by descriptor number, matching the task's
//! descriptor table.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::task::Fd;

/// `struct epoll_event` bits (Linux values).
pub const EPOLLIN: u32 = 0x0000_0001;
pub const EPOLLOUT: u32 = 0x0000_0004;
pub const EPOLLERR: u32 = 0x0000_0008;
pub const EPOLLHUP: u32 = 0x0000_0010;
pub const EPOLLRDHUP: u32 = 0x0000_2000;
pub const EPOLLET: u32 = 0x8000_0000;

const REPORT_ALWAYS: u32 = EPOLLERR | EPOLLHUP;

/// One registered descriptor.
#[derive(Clone)]
struct Interest {
    fd: usize,
    target: Fd,
    events: u32,
    data: u64,
    /// Revents at the end of the last `epoll_wait` (edge bookkeeping).
    last_revents: u16,
    /// Handle freshness at the end of the last `epoll_wait`.
    last_gen: u64,
}

/// An epoll instance behind `epoll_create1`.
pub struct Epoll {
    interests: Mutex<Vec<Interest>>,
    nonblock: AtomicBool,
}

impl Epoll {
    /// Create an empty instance.
    pub fn new() -> Arc<Epoll> {
        Arc::new(Epoll {
            interests: Mutex::new(Vec::new()),
            nonblock: AtomicBool::new(false),
        })
    }

    /// `O_NONBLOCK` of the epoll fd.
    pub fn nonblock(&self) -> bool {
        self.nonblock.load(Ordering::Acquire)
    }

    /// Set `O_NONBLOCK` on the epoll fd.
    pub fn set_nonblock(&self, on: bool) {
        self.nonblock.store(on, Ordering::Release);
    }

    /// `EPOLL_CTL_ADD`: register `fd`/`target`. The caller already checked the
    /// descriptor is open; a duplicate registration is rejected.
    pub fn add(&self, fd: usize, target: Fd, events: u32, data: u64) -> Result<(), ()> {
        let mut interests = self.interests.lock();
        if interests.iter().any(|interest| interest.fd == fd) {
            return Err(());
        }
        interests.push(Interest {
            fd,
            target,
            events,
            data,
            last_revents: 0,
            last_gen: 0,
        });
        Ok(())
    }

    /// `EPOLL_CTL_MOD`: replace the events/data of a registered descriptor.
    pub fn modify(&self, fd: usize, events: u32, data: u64) -> Result<(), ()> {
        let mut interests = self.interests.lock();
        let Some(interest) = interests.iter_mut().find(|interest| interest.fd == fd) else {
            return Err(());
        };
        interest.events = events;
        interest.data = data;
        // Re-arm: the next wait reports the new interest set's current state.
        interest.last_revents = 0;
        interest.last_gen = 0;
        Ok(())
    }

    /// `EPOLL_CTL_DEL`: drop a registered descriptor.
    pub fn delete(&self, fd: usize) -> Result<(), ()> {
        let mut interests = self.interests.lock();
        let before = interests.len();
        interests.retain(|interest| interest.fd != fd);
        if interests.len() == before {
            return Err(());
        }
        Ok(())
    }

    /// Drop `fd`'s interest because the user closed the descriptor. Called by
    /// `task::fd_close`, so a reused descriptor number cannot inherit a stale
    /// registration.
    pub fn drop_fd(&self, fd: usize) {
        self.interests.lock().retain(|interest| interest.fd != fd);
    }

    /// One `epoll_wait` pass: at most `max` `(events, data)` pairs. Ready
    /// interests are found by polling each handle outside the list lock, then
    /// the edge-trigger bookkeeping is written back.
    pub fn ready(&self, max: usize) -> Vec<(u32, u64)> {
        // Clone the list so handle polls (which may take the task table for a
        // terminal) run without the interests lock.
        let snapshot: Vec<Interest> = self.interests.lock().clone();
        // (fd, revents, freshness) after this scan.
        let mut scanned: Vec<(usize, u16, u64)> = Vec::with_capacity(snapshot.len());
        let mut out: Vec<(u32, u64)> = Vec::new();
        for interest in &snapshot {
            let (revents, gen) = interest.target.poll_gen((interest.events & 0xffff) as u16);
            scanned.push((interest.fd, revents, gen));
            if out.len() == max {
                continue;
            }
            let reportable = (revents as u32) & (interest.events | REPORT_ALWAYS);
            if reportable == 0 {
                continue;
            }
            let edge = interest.events & EPOLLET != 0;
            let fresh = gen != interest.last_gen || revents != interest.last_revents;
            if !edge || fresh {
                out.push((reportable, interest.data));
            }
        }
        {
            let mut interests = self.interests.lock();
            for (fd, revents, gen) in scanned {
                if let Some(interest) = interests.iter_mut().find(|interest| interest.fd == fd) {
                    interest.last_revents = revents;
                    interest.last_gen = gen;
                }
            }
        }
        out
    }

    /// Whether any interest is ready right now (`poll` on the epoll fd).
    pub fn has_ready(&self) -> bool {
        let snapshot: Vec<Interest> = self.interests.lock().clone();
        snapshot.iter().any(|interest| {
            let (revents, _) = interest.target.poll_gen((interest.events & 0xffff) as u16);
            (revents as u32) & (interest.events | REPORT_ALWAYS) != 0
        })
    }

    /// Number of registered descriptors (tests/diagnostics).
    pub fn len(&self) -> usize {
        self.interests.lock().len()
    }

    /// Whether nothing is registered (tests/diagnostics).
    pub fn is_empty(&self) -> bool {
        self.interests.lock().is_empty()
    }
}
