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
//! stays acyclic (two carried ends may still lead to one pair).
//!
//! The check is exact: queueing `fds` on `dest` is refused only when `dest`
//! is one of the carried sockets or reachable from them. A pair whose queue
//! already holds descriptors still travels when none of them leads back
//! (issue #865: WebKit's NetworkProcess sends the service-worker store's
//! shared memory on a new connection while the connection's other end is on
//! its way to the UI process; refusing that end lost the reply the WebProcess
//! waits for, and its page hung until WebKit killed the NetworkProcess, or for
//! good). An epoll instance never travels: it can gain a watched socket after
//! it is in flight, so its edges cannot be checked once.
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
//!
//! Cost: each socket is expanded once per walk (a mark of the walk's number,
//! [`EPOCH`], which only a walk holding [`GRAPH`] writes), so a walk reads
//! every reachable queue once: at most the live pairs ([`super::MAX_PIPES`]
//! halves), the listeners and the descriptors in flight.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::{Mutex, MutexGuard};

use super::{Rights, SocketPair};
use crate::ipc::unix::Listener;
use crate::task::Fd;

/// Serialises the edge-adding enqueues (see the module comment).
static GRAPH: Mutex<()> = Mutex::new(());

/// The number of the current (or last) walk; marks equal to it are visited.
static EPOCH: AtomicU64 = AtomicU64::new(0);

/// Most distinct sockets one walk expands before it refuses: far more than
/// the live pairs allow, so only a crowd of listeners in flight reaches it.
const MAX_NODES: usize = 1024;

/// A socket the walk passes through.
#[derive(Clone)]
pub(super) enum Node {
    Pair(Arc<SocketPair>),
    Listener(Arc<Listener>),
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

    /// The mark of the walk that last met this socket.
    fn mark(&self) -> &AtomicU64 {
        match self {
            Node::Pair(pair) => &pair.walk,
            Node::Listener(listener) => &listener.walk,
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
enum Plan {
    /// Nothing that holds a socket: no edge, no check.
    Free,
    /// Hold [`GRAPH`] and walk.
    Walk,
}

/// Why a send of descriptors was refused (`PROC:SCM:REFUSE why=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The sender's in-flight quota or the global ceiling is spent.
    Quota,
    /// Queueing them would close a cycle.
    Cycle,
    /// An epoll instance never travels.
    Epoll,
    /// The walk met more than [`MAX_NODES`] sockets or could not allocate.
    Limit,
}

impl Refusal {
    fn name(self) -> &'static str {
        match self {
            Refusal::Quota => "quota",
            Refusal::Cycle => "cycle",
            Refusal::Epoll => "epoll",
            Refusal::Limit => "limit",
        }
    }
}

/// How queueing `rights` must be checked.
fn plan(rights: &Rights) -> Result<Plan, Refusal> {
    let mut walk = false;
    for fd in rights.fds() {
        match fd {
            Fd::Epoll { .. } => return Err(Refusal::Epoll),
            Fd::Socket { .. } | Fd::UnixListener { .. } => walk = true,
            _ => {}
        }
    }
    Ok(if walk { Plan::Walk } else { Plan::Free })
}

/// What queueing `rights` on `dest` (the pair a direction belongs to; none
/// for a pipe) needs: `Ok(None)` when it adds no edge, `Ok(Some(lock))` when
/// the walk found no cycle (keep the lock until the descriptors are queued),
/// or why it is refused (the lock is released by then). What the walk met is
/// left in `held` (see [`check`]).
pub(super) fn admit(
    rights: Option<&Rights>,
    dest: Option<&SocketPair>,
    held: &mut Vec<Node>,
) -> Result<Option<MutexGuard<'static, ()>>, Refusal> {
    let (Some(rights), Some(dest)) = (rights, dest) else {
        return Ok(None);
    };
    match plan(rights)? {
        Plan::Free => Ok(None),
        Plan::Walk => {
            let graph = GRAPH.lock();
            check(rights, dest, held)?;
            Ok(Some(graph))
        }
    }
}

/// Whether queueing `rights` on `dest` may go ahead: `Err` when `dest` is
/// reachable from them (or the walk could not finish). Called with [`GRAPH`]
/// held and no queue lock. Every socket the walk expands is appended to
/// `held` once (a breadth-first queue that never shrinks), for the caller to
/// drop once its locks are released. A walk that cannot allocate, or meets
/// more than [`MAX_NODES`] sockets, refuses: a refused send is recoverable, a
/// cycle is not.
fn check(rights: &Rights, dest: &SocketPair, held: &mut Vec<Node>) -> Result<(), Refusal> {
    // Walks are serialised by GRAPH: nobody else writes a mark meanwhile.
    let epoch = EPOCH.fetch_add(1, Ordering::Relaxed) + 1;
    let start = held.len();
    let mut ok = true;
    for fd in rights.fds() {
        ok = ok && push(held, epoch, fd);
    }
    let mut next = start;
    while ok && next < held.len() {
        if held.len() - start > MAX_NODES {
            return Err(Refusal::Limit);
        }
        let node = held[next].clone();
        next += 1;
        if let Node::Pair(pair) = &node {
            if core::ptr::eq(&**pair, dest) {
                return Err(Refusal::Cycle);
            }
        }
        ok = match &node {
            Node::Pair(pair) => pair.queued_fds(|fd| push(held, epoch, fd)),
            Node::Listener(listener) => {
                listener.pending_each(|pair| push_node(held, epoch, Node::Pair(Arc::clone(pair))))
            }
        };
        crate::arch::irq_window::poll_point();
    }
    if ok {
        Ok(())
    } else {
        Err(Refusal::Limit)
    }
}

/// Append the node `fd` stands for, unless this walk met it already; false
/// when `held` cannot grow.
fn push(held: &mut Vec<Node>, epoch: u64, fd: &Fd) -> bool {
    match Node::of(fd) {
        Some(node) => push_node(held, epoch, node),
        None => true,
    }
}

fn push_node(held: &mut Vec<Node>, epoch: u64, node: Node) -> bool {
    if node.mark().swap(epoch, Ordering::Relaxed) == epoch {
        return true;
    }
    held.try_reserve(1).is_ok() && {
        held.push(node);
        true
    }
}

/// Refused sends of descriptors, a line each while the budget lasts.
static REFUSALS: tracefmt::Budget = tracefmt::Budget::new(20, 1000);
/// Every refusal of the boot, for the `count=` field.
static REFUSED: AtomicU64 = AtomicU64::new(0);

/// Print `PROC:SCM:REFUSE` for a refused `SCM_RIGHTS` send of the current
/// task. A program usually just loses the message (musl calls `ETOOMANYREFS`
/// "No error information"), so the kernel says which process it was and why
/// (issue #865). Call it with no lock held: it reads the task table.
pub fn note_refusal(why: Refusal) {
    let count = REFUSED.fetch_add(1, Ordering::Relaxed) + 1;
    match REFUSALS.charge(crate::task::ticks()) {
        tracefmt::Charge::Deny => return,
        tracefmt::Charge::AllowAfter(n) => {
            crate::trace::line!(tracefmt::Channel::Proc, "SCM:SUPPRESSED n={n}");
        }
        tracefmt::Charge::Allow => {}
    }
    let me = crate::task::current();
    let (comm, len) = crate::task::linuxstate::comm(me);
    crate::trace::line!(
        tracefmt::Channel::Proc,
        "SCM:REFUSE why={} tgid={} count={} comm={}",
        why.name(),
        crate::task::linuxstate::tgid(),
        count,
        core::str::from_utf8(&comm[..len]).unwrap_or("?")
    );
}
