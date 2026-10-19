//! Whether queueing descriptors on a socket would close a cycle of sockets
//! carrying each other (`SCM_RIGHTS`).
//!
//! Sockets in flight form a graph: a socket pair whose queue holds an end of
//! another pair (or a listener, which holds the pairs of the connections
//! nobody accepted) has an edge to it. A cycle in that graph is a set of
//! sockets only reachable from each other's queues: nothing can ever read or
//! purge them, so their in-flight budget leaks for the rest of the boot.
//! Linux reclaims such sets with a garbage collector; there is none here, so
//! the edge that would close a cycle is refused (`ETOOMANYREFS`) and the graph
//! stays a forest.
//!
//! The check is exact: queueing `fds` on `dest` is refused only when `dest`
//! is reachable from them. A pair whose queue already holds descriptors still
//! travels when none of them leads back (issue #865: WebKit's NetworkProcess
//! sends the service-worker store's shared memory on a new connection while
//! the connection's other end is on its way to the UI process; refusing that
//! end lost the reply the WebProcess waits for, and its page hung until WebKit
//! killed the NetworkProcess). An epoll instance never travels: it can gain a
//! watched socket after it is in flight, so its edges cannot be checked once.
//!
//! Locking: every enqueue that adds an edge (descriptors that hold sockets)
//! holds [`GRAPH`] from its walk to its enqueue, so two sends cannot each
//! pass the check and together close a cycle; edges that go away meanwhile
//! (a read, a close) only make the answer more cautious. The walk takes one
//! queue lock at a time and runs before the destination's ring is locked, and
//! what it collected is dropped by the caller after both locks are released
//! (dropping the last reference to a socket closes it, which takes other
//! locks). A new connection adds the edge listener -> new pair without the
//! lock: the new pair's queues are empty, so it cannot close a cycle.

use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, MutexGuard};

use super::{Rights, SocketPair};
use crate::ipc::unix::Listener;
use crate::task::Fd;

/// Serialises the edge-adding enqueues (see the module comment).
static GRAPH: Mutex<()> = Mutex::new(());

/// Most sockets one walk visits before it refuses: far more than any real
/// program parks in flight, and a bound on the time [`GRAPH`] is held.
const MAX_NODES: usize = 4096;

/// A socket the walk passes through.
pub(super) enum Node {
    Pair(Arc<SocketPair>),
    Listener(Arc<Listener>),
}

impl Clone for Node {
    fn clone(&self) -> Node {
        match self {
            Node::Pair(pair) => Node::Pair(Arc::clone(pair)),
            Node::Listener(listener) => Node::Listener(Arc::clone(listener)),
        }
    }
}

impl Node {
    /// The node a carried descriptor stands for, if it holds sockets.
    fn of(fd: &Fd) -> Option<Node> {
        match fd {
            Fd::Socket { pair, .. } => Some(Node::Pair(Arc::clone(pair))),
            Fd::UnixListener { listener } => Some(Node::Listener(Arc::clone(listener))),
            _ => None,
        }
    }

    fn addr(&self) -> usize {
        match self {
            Node::Pair(pair) => Arc::as_ptr(pair) as usize,
            Node::Listener(listener) => Arc::as_ptr(listener) as usize,
        }
    }
}

impl SocketPair {
    /// [`super::Pipe::queued_fds`] of both directions, one ring lock at a time.
    fn queued_fds(&self, mut each: impl FnMut(&Fd) -> bool) -> bool {
        let (read, write) = self.directions(super::Side::A);
        read.queued_fds(&mut each) && write.queued_fds(&mut each)
    }
}

/// What a send of `rights` needs before its descriptors may be queued.
pub(super) enum Plan {
    /// Nothing that holds a socket: no edge, no check.
    Free,
    /// Hold [`GRAPH`] and walk.
    Walk,
    /// An epoll instance: refused outright.
    Refuse,
}

/// How queueing `rights` must be checked.
pub(super) fn plan(rights: &Rights) -> Plan {
    let mut walk = false;
    for fd in rights.fds() {
        match fd {
            Fd::Epoll { .. } => return Plan::Refuse,
            Fd::Socket { .. } | Fd::UnixListener { .. } => walk = true,
            _ => {}
        }
    }
    if walk {
        Plan::Walk
    } else {
        Plan::Free
    }
}

/// The lock an edge-adding enqueue holds from its walk to its enqueue.
pub(super) fn lock() -> MutexGuard<'static, ()> {
    GRAPH.lock()
}

/// Whether `dest` is reachable from `rights` (the send would close a cycle).
/// Called with [`lock`] held and no queue lock. Every socket the walk meets
/// is appended to `held` (a breadth-first queue that never shrinks), for the
/// caller to drop once its locks are released. A walk that cannot allocate,
/// or meets more than [`MAX_NODES`] sockets, answers yes: refusing a send is
/// recoverable, a cycle is not.
pub(super) fn reaches(rights: &Rights, dest: &SocketPair, held: &mut Vec<Node>) -> bool {
    let start = held.len();
    if !rights.fds().iter().all(|fd| push(held, fd)) {
        return true;
    }
    let mut next = start;
    while next < held.len() {
        if held.len() - start > MAX_NODES {
            return true;
        }
        let node = held[next].clone();
        let addr = node.addr();
        next += 1;
        if let Node::Pair(pair) = &node {
            if core::ptr::eq(&**pair, dest) {
                return true;
            }
        }
        // Met before (the graph is a forest, but two carried descriptors may
        // lead to one socket): expanded once is enough.
        if held[start..next - 1].iter().any(|old| old.addr() == addr) {
            continue;
        }
        let complete = match &node {
            Node::Pair(pair) => pair.queued_fds(|fd| push(held, fd)),
            Node::Listener(listener) => listener.pending_each(|pair| push_pair(held, pair)),
        };
        if !complete {
            return true;
        }
    }
    false
}

/// Append the node `fd` stands for; false when `held` cannot grow.
fn push(held: &mut Vec<Node>, fd: &Fd) -> bool {
    match Node::of(fd) {
        Some(node) => {
            held.try_reserve(1).is_ok() && {
                held.push(node);
                true
            }
        }
        None => true,
    }
}

/// Append a listener's pending connection; false when `held` cannot grow.
fn push_pair(held: &mut Vec<Node>, pair: &Arc<SocketPair>) -> bool {
    held.try_reserve(1).is_ok() && {
        held.push(Node::Pair(Arc::clone(pair)));
        true
    }
}
