//! [`InetSock`]: one application-side `AF_INET` socket.
//!
//! The blocking control calls (`bind`, `connect`, `listen`) are a *begin* that
//! queues the request and a *finish* that waits for `netd`'s answer; the split
//! lets the in-kernel tests play `netd` between the two without a scheduler.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

use crate::ipc::pipe::{Mode, Side, SocketPair, POLLERR, POLLHUP, POLLIN, POLLOUT};
use crate::task::wait::WaitQueue;
use crate::task::{self, WaitKind, WakeReason};

use super::errno::*;
use super::timeout::{Dir, Timeouts};
use super::{Addr, Kind, Op, Request, State, ACCEPT_QUEUE, TABLE};

/// Ticks (100 Hz) a control call waits for `netd` before `ETIMEDOUT`. `netd`
/// bounds its own connection attempts at 60 s, so this is longer than that.
const CONTROL_WAIT_TICKS: u64 = 7_000;

pub(super) struct Inner {
    pub state: State,
    pub local: Addr,
    pub peer: Addr,
    /// The application's side (B) of the data path.
    pub pair: Option<Arc<SocketPair>>,
    /// Connections `netd` accepted, not yet taken by `accept`.
    pub accept_q: VecDeque<Arc<InetSock>>,
    /// The operation `netd` is working on.
    pub pending: Option<Op>,
    /// Its answer, until the waiter takes it.
    pub done: Option<Result<(), i32>>,
    /// A failure to report through `SO_ERROR` (a non-blocking `connect`).
    pub so_error: i32,
    /// A `connect` returned (`EINPROGRESS`, interrupted, timed out) before
    /// `netd` answered: nobody will collect the answer, so a failure goes to
    /// `SO_ERROR` and `poll`, whatever the socket's mode is by then (std's
    /// `connect_timeout` switches the socket back to blocking right away).
    pub detached: bool,
}

pub struct InetSock {
    pub(super) id: u32,
    kind: Kind,
    pub(super) inner: Mutex<Inner>,
    pub(super) wq: WaitQueue,
    nonblock: AtomicBool,
    /// `SO_RCVTIMEO` and `SO_SNDTIMEO`.
    timeouts: Timeouts,
    /// Bumped on every state change, for `epoll` edge triggering.
    pub(super) events: AtomicU64,
}

/// A queued control call, to be finished with [`InetSock::finish`].
#[derive(Clone, Copy, Debug)]
pub struct Ticket(Op);

impl InetSock {
    pub(super) fn new(id: u32, kind: Kind) -> Arc<InetSock> {
        Arc::new(InetSock {
            id,
            kind,
            inner: Mutex::new(Inner {
                state: State::Fresh,
                local: Addr::ANY,
                peer: Addr::ANY,
                pair: None,
                accept_q: VecDeque::new(),
                pending: None,
                done: None,
                so_error: 0,
                detached: false,
            }),
            wq: WaitQueue::new(WaitKind::Pipe),
            nonblock: AtomicBool::new(false),
            timeouts: Timeouts::new(),
            events: AtomicU64::new(0),
        })
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn state(&self) -> State {
        self.inner.lock().state
    }

    pub fn local(&self) -> Addr {
        self.inner.lock().local
    }

    pub fn peer(&self) -> Option<Addr> {
        let inner = self.inner.lock();
        (inner.state == State::Connected || self.kind == Kind::Dgram && inner.peer != Addr::ANY)
            .then_some(inner.peer)
    }

    pub fn nonblock(&self) -> bool {
        self.nonblock.load(Ordering::Acquire)
    }

    /// Set `O_NONBLOCK`, on the data path too once there is one.
    pub fn set_nonblock(&self, on: bool) {
        self.nonblock.store(on, Ordering::Release);
        if let Some(pair) = &self.inner.lock().pair {
            pair.set_nonblock(Side::B, on);
        }
    }

    /// `SO_RCVTIMEO` or `SO_SNDTIMEO` in ticks; `None` is no timeout.
    pub fn timeout(&self, dir: Dir) -> Option<u64> {
        self.timeouts.get(dir)
    }

    pub fn set_timeout(&self, dir: Dir, ticks: Option<u64>) {
        self.timeouts.set(dir, ticks);
    }

    /// The absolute tick a blocking call in `dir` starting now gives up at.
    pub fn deadline(&self, dir: Dir) -> Option<u64> {
        self.timeouts.deadline(dir)
    }

    /// The application's data path, if the socket has one.
    pub fn pair(&self) -> Option<Arc<SocketPair>> {
        self.inner.lock().pair.clone()
    }

    /// `SO_ERROR`: the pending error, which reading clears.
    pub fn take_error(&self) -> i32 {
        core::mem::take(&mut self.inner.lock().so_error)
    }

    fn post(&self, op: Op) -> bool {
        TABLE.lock().push(Request {
            id: self.id,
            kind: self.kind,
            op,
        })
    }

    /// Queue `op` for `netd`. `EALREADY` when it is still busy with a
    /// previous one (this socket's earlier call was interrupted or timed out).
    fn begin(&self, op: Op) -> Result<Ticket, i32> {
        let mut inner = self.inner.lock();
        if inner.pending.is_some() {
            return Err(EALREADY);
        }
        inner.pending = Some(op);
        inner.done = None;
        if matches!(op, Op::Connect(_)) {
            inner.state = State::Connecting;
        }
        drop(inner);
        if !self.post(op) {
            let mut inner = self.inner.lock();
            inner.pending = None;
            if inner.state == State::Connecting {
                inner.state = State::Fresh;
            }
            return Err(ENOBUFS);
        }
        Ok(Ticket(op))
    }

    /// Wait for `netd`'s answer to `ticket`. A non-blocking socket does not
    /// wait: `connect` reports `EINPROGRESS`, the others `EAGAIN`. A blocking
    /// `connect` waits no longer than `SO_SNDTIMEO` either, and then reports
    /// `EINPROGRESS` too while the connection goes on, as Linux does.
    pub fn finish(&self, ticket: Ticket) -> Result<(), i32> {
        let control = task::ticks() + CONTROL_WAIT_TICKS;
        let send = match ticket.0 {
            Op::Connect(_) => self.deadline(Dir::Send).filter(|&at| at < control),
            _ => None,
        };
        let deadline = send.unwrap_or(control);
        loop {
            if let Some(answer) = self.inner.lock().done.take() {
                return answer;
            }
            // Only `connect` returns early on a non-blocking socket: bind and
            // listen are a short round trip to `netd` whatever the flags.
            if self.nonblock() && matches!(ticket.0, Op::Connect(_)) {
                self.inner.lock().detached = true;
                return Err(EINPROGRESS);
            }
            // The in-kernel suite has no scheduler, so it plays `netd` here.
            #[cfg(lazyos_tests)]
            if let Some(responder) = *super::RESPONDER.lock() {
                responder();
                if self.inner.lock().done.is_some() {
                    continue;
                }
                // Nobody will ever answer: fail instead of waiting for a
                // scheduler that is not running.
                return Err(ETIMEDOUT);
            }
            if send.is_some() && task::ticks() >= deadline {
                return Err(self.detach(ticket, EINPROGRESS));
            }
            match self.wq.wait(task::current(), Some(deadline)) {
                WakeReason::Woken => {}
                // Look once more: the answer may have come with the deadline.
                WakeReason::TimedOut if send.is_some() => {}
                WakeReason::TimedOut => return Err(self.detach(ticket, ETIMEDOUT)),
                WakeReason::Interrupted => return Err(self.detach(ticket, EINTR)),
            }
        }
    }

    /// The caller gives up on `ticket` with `error`; a `connect` goes on.
    fn detach(&self, ticket: Ticket, error: i32) -> i32 {
        if matches!(ticket.0, Op::Connect(_)) {
            self.inner.lock().detached = true;
        }
        error
    }

    // ---- the calls ------------------------------------------------------------

    pub fn begin_bind(&self, addr: Addr) -> Result<Ticket, i32> {
        if self.inner.lock().state != State::Fresh {
            return Err(EINVAL);
        }
        self.begin(Op::Bind(addr))
    }

    pub fn begin_connect(&self, addr: Addr) -> Result<Option<Ticket>, i32> {
        if addr.port == 0 {
            return Err(EINVAL);
        }
        let (state, bound) = {
            let inner = self.inner.lock();
            (inner.state, inner.pair.is_some())
        };
        match state {
            State::Connected => return Err(EISCONN),
            State::Connecting => return Err(EALREADY),
            State::Listening => return Err(EINVAL),
            State::Fresh | State::Bound => {}
        }
        if self.kind == Kind::Dgram {
            // A datagram "connection" only fixes the default peer; the socket
            // is bound first if it is not (an ephemeral port).
            if bound {
                self.inner.lock().peer = addr;
                return Ok(None);
            }
            let ticket = self.begin(Op::Bind(Addr::ANY))?;
            self.inner.lock().peer = addr;
            return Ok(Some(ticket));
        }
        self.begin(Op::Connect(addr)).map(Some)
    }

    pub fn begin_listen(&self, backlog: u32) -> Result<Ticket, i32> {
        if self.kind != Kind::Stream {
            return Err(EOPNOTSUPP);
        }
        match self.inner.lock().state {
            State::Fresh | State::Bound => {}
            State::Listening => return Err(EINVAL),
            _ => return Err(EINVAL),
        }
        self.begin(Op::Listen(backlog.clamp(1, 8)))
    }

    /// Take the next accepted connection, waiting for one unless non-blocking
    /// (and, with `SO_RCVTIMEO`, no longer than that: then `EAGAIN`).
    pub fn accept(&self) -> Result<Arc<InetSock>, i32> {
        let deadline = self.deadline(Dir::Recv);
        loop {
            {
                let mut inner = self.inner.lock();
                if inner.state != State::Listening {
                    return Err(EINVAL);
                }
                if let Some(conn) = inner.accept_q.pop_front() {
                    return Ok(conn);
                }
            }
            if self.nonblock() || deadline.is_some_and(|at| task::ticks() >= at) {
                return Err(EAGAIN);
            }
            match self.wq.wait(task::current(), deadline) {
                WakeReason::Interrupted => return Err(EINTR),
                WakeReason::Woken | WakeReason::TimedOut => {}
            }
        }
    }

    /// `shutdown(2)` on a connected socket.
    pub fn shutdown(&self, how: u64) -> Result<(), i32> {
        match self.pair() {
            Some(pair) if pair.shutdown(Side::B, how) => Ok(()),
            Some(_) => Err(EINVAL),
            None => Err(ENOTCONN),
        }
    }

    /// `poll` readiness plus the freshness counter for `epoll` edges.
    pub fn poll_gen(&self, events: u16) -> (u16, u64) {
        let inner = self.inner.lock();
        let own = self.events.load(Ordering::Acquire);
        if let Some(pair) = &inner.pair {
            let (mut revents, gen) = pair.poll_gen(Side::B, events);
            if inner.so_error != 0 {
                revents |= POLLERR | POLLHUP;
            }
            // Separate bit ranges: a pair event and a socket event that land
            // together must not cancel (an XOR of two even counters would).
            return (revents, gen.wrapping_add(own << 32));
        }
        let mut revents = 0;
        match inner.state {
            State::Listening if events & POLLIN != 0 && !inner.accept_q.is_empty() => {
                revents |= POLLIN;
            }
            // A failed connect reports the error and is writable, as on Linux.
            _ if inner.so_error != 0 => revents |= POLLERR | POLLHUP | (events & POLLOUT),
            State::Fresh | State::Bound => revents |= events & POLLOUT,
            _ => {}
        }
        (revents, own)
    }

    pub(super) fn bump(&self) {
        self.events.fetch_add(1, Ordering::AcqRel);
        self.wq.notify_all();
        task::notify_poll();
    }

    /// The pair `netd` completes a connection with.
    pub(super) fn new_pair(&self) -> Option<Arc<SocketPair>> {
        let mode = match self.kind {
            Kind::Stream => Mode::Stream,
            Kind::Dgram => Mode::Seqpacket,
        };
        SocketPair::new_small(mode)
    }

    /// Queue a connection `netd` accepted. `false` when the queue is full.
    pub(super) fn offer(&self, conn: Arc<InetSock>) -> bool {
        let mut inner = self.inner.lock();
        if inner.state != State::Listening || inner.accept_q.len() >= ACCEPT_QUEUE {
            return false;
        }
        inner.accept_q.push_back(conn);
        drop(inner);
        self.bump();
        true
    }
}

impl Drop for InetSock {
    fn drop(&mut self) {
        // The application's reference on its side of the data path; `netd` sees
        // the end of the stream and the queued close.
        if let Some(pair) = self.inner.get_mut().pair.take() {
            pair.close(Side::B);
        }
        let mut table = TABLE.lock();
        let known = table
            .get_mut(self.id)
            .map(|slot| slot.closed = true)
            .is_some();
        if known {
            table.push(Request {
                id: self.id,
                kind: self.kind,
                op: Op::Close,
            });
        }
    }
}
