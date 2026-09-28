//! Pathname `AF_UNIX` sockets (Linux ABI round 2).
//!
//! `socket(AF_UNIX, SOCK_STREAM)` + `bind` + `listen` + `connect` + `accept`
//! for `std::os::unix::net::{UnixListener, UnixStream}`. Bound names live in a
//! kernel registry, not the VFS: LazyOS has no socket inode type, so a name is
//! a path (or abstract) byte string that `connect` resolves to the listener.
//! Closing the listening socket unregisters the name (Linux would leave the
//! filesystem entry until `unlink`); that keeps rebinding deterministic within
//! a boot.
//!
//! Accepted connections reuse [`SocketPair`](crate::ipc::pipe::SocketPair):
//! `connect` creates the pair, queues the server side on the listener, and
//! `accept` pops it into a descriptor. A full pending queue simply parks in
//! the accept path on the listener's wait queue.

use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

use crate::ipc::pipe::{SocketPair, POLLIN};
use crate::task::wait::WaitQueue;
use crate::task::{WaitKind, WakeReason};

/// A bound, listening `AF_UNIX` name.
pub struct Listener {
    /// The bound name: filesystem path bytes, or an abstract name whose first
    /// byte is NUL (kept verbatim, so it can never collide with a path).
    pub name: Vec<u8>,
    /// Set by `listen(2)`; `connect` to a bound but unlistening name fails.
    listening: AtomicBool,
    /// `O_NONBLOCK` of the listening descriptor.
    nonblock: AtomicBool,
    /// Server-side pair halves delivered by `connect` and not yet accepted.
    pending: Mutex<VecDeque<Arc<SocketPair>>>,
    /// Accept callers parked while no connection is pending.
    accept_wq: WaitQueue,
    /// Connections delivered so far, the freshness counter for `epoll`.
    connects: AtomicU64,
}

impl Listener {
    fn new(name: Vec<u8>) -> Arc<Listener> {
        Arc::new(Listener {
            name,
            listening: AtomicBool::new(false),
            nonblock: AtomicBool::new(false),
            pending: Mutex::new(VecDeque::new()),
            accept_wq: WaitQueue::new(WaitKind::UnixAccept),
            connects: AtomicU64::new(0),
        })
    }

    /// `listen(2)`: mark the name connectable.
    pub fn listen(&self) {
        self.listening.store(true, Ordering::Release);
    }

    /// Whether `listen(2)` has been called.
    pub fn is_listening(&self) -> bool {
        self.listening.load(Ordering::Acquire)
    }

    /// `O_NONBLOCK` of the listener.
    pub fn nonblock(&self) -> bool {
        self.nonblock.load(Ordering::Acquire)
    }

    /// Set `O_NONBLOCK` on the listener.
    pub fn set_nonblock(&self, on: bool) {
        self.nonblock.store(on, Ordering::Release);
    }

    /// Deliver a client connection: queue the server side and wake an accepter.
    pub fn connect(&self, pair: Arc<SocketPair>) {
        self.pending.lock().push_back(pair);
        self.connects.fetch_add(1, Ordering::AcqRel);
        self.accept_wq.notify_all();
        crate::task::notify_poll();
    }

    /// Take the oldest pending connection, if any.
    pub fn take_pending(&self) -> Option<Arc<SocketPair>> {
        self.pending.lock().pop_front()
    }

    /// Whether a connection is waiting to be accepted.
    pub fn has_pending(&self) -> bool {
        !self.pending.lock().is_empty()
    }

    /// Park the current task until a connection arrives.
    pub fn wait_connection(&self) -> WakeReason {
        self.accept_wq.wait(crate::task::current(), None)
    }

    /// `poll` revents for the listener (`POLLIN` when accept would not block).
    pub fn poll(&self, events: u16) -> u16 {
        if events & POLLIN != 0 && self.has_pending() {
            POLLIN
        } else {
            0
        }
    }

    /// [`poll`](Listener::poll) plus the connect counter for `EPOLLET`.
    pub fn poll_gen(&self, events: u16) -> (u16, u64) {
        (self.poll(events), self.connects.load(Ordering::Acquire))
    }
}

/// Every bound name in the system. A short `Vec` scan: the counts stay tiny.
/// Entries hold a `Weak` handle because a bound name disappears with its last
/// descriptor: `dup`/`fork` clones must not unregister it, and the registry
/// must not keep the listener alive either.
static LISTENERS: Mutex<Vec<(Vec<u8>, Weak<Listener>)>> = Mutex::new(Vec::new());

/// Drop entries whose last descriptor is gone.
fn prune(listeners: &mut Vec<(Vec<u8>, Weak<Listener>)>) {
    listeners.retain(|(_, listener)| listener.strong_count() > 0);
}

/// Bind `name`, rejecting a duplicate (`-EADDRINUSE`).
pub fn bind(name: Vec<u8>) -> Result<Arc<Listener>, ()> {
    let mut listeners = LISTENERS.lock();
    prune(&mut listeners);
    if listeners.iter().any(|(bound, _)| *bound == name) {
        return Err(());
    }
    let listener = Listener::new(name.clone());
    listeners.push((name, Arc::downgrade(&listener)));
    Ok(listener)
}

/// The listener bound to `name`, if any.
pub fn lookup(name: &[u8]) -> Option<Arc<Listener>> {
    let mut listeners = LISTENERS.lock();
    prune(&mut listeners);
    listeners
        .iter()
        .find(|(bound, _)| bound.as_slice() == name)
        .and_then(|(_, listener)| listener.upgrade())
}

/// Empty the registry (test isolation).
#[cfg(laZYOS_TESTS)]
pub fn clear_for_test() {
    LISTENERS.lock().clear();
}

/// Number of live bound names (tests/diagnostics).
pub fn bound_count() -> usize {
    let mut listeners = LISTENERS.lock();
    prune(&mut listeners);
    listeners.len()
}
