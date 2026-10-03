//! `AF_INET` sockets for the Linux ABI (docs/networking-plan.md, stage N5).
//!
//! The kernel has no TCP/IP: the stack is the userspace service `netd`. This
//! module is the seam between the two, the "kernel socket object with `netd`
//! behind it" of the plan's option L2.
//!
//! * An application's socket is an [`InetSock`] held by an `Fd::Inet`. Once it
//!   has a connection, its data path is a [`SocketPair`] of small rings: the
//!   application owns side B, so `read`, `write`, `poll`, `epoll`, `dup`,
//!   `fork` and `shutdown` are the ones the kernel already has for sockets.
//!   A datagram socket's pair is `SOCK_SEQPACKET`; each message starts with
//!   the peer's address (see [`DGRAM_HEADER`]).
//! * `netd` owns side A. It never sleeps in the kernel on a socket: the
//!   control operations that need the network (`bind`, `connect`, `listen`)
//!   are *requests* queued here, which `netd` fetches ([`pump::next_request`])
//!   and answers ([`pump::complete`]); the caller parks on the socket's wait
//!   queue until then. Connections `netd` accepts are handed in
//!   ([`pump::accepted`]). Bytes move through side A with non-blocking reads
//!   and writes, and `netd` polls on its own tick, so no kernel-originated
//!   Messenger call is needed.
//!
//! **Ownership** is the file descriptor: it is the kernel's, so `fork`, `dup`
//! and passing a socket to a child need nothing from `netd` (the reason the
//! plan rejected per-call proxying).
//!
//! **Bounds.** At most [`MAX_SOCKETS`] sockets (live or waiting for `netd` to
//! acknowledge their close), [`MAX_REQUESTS`] queued requests, [`ACCEPT_QUEUE`]
//! connections waiting in a listener, and two [`SMALL_CAPACITY`] rings each.
//!
//! **Not done.** If `netd` dies, sockets with a connection see EOF only when
//! the stack's side closes; they are not torn down. A restarted `netd` calls
//! [`pump::attach`], which discards everything left from the old one.

use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use spin::Mutex;

use crate::ipc::pipe::SocketPair;

mod pump;
mod sock;
mod timeout;

pub use pump::*;
pub use sock::InetSock;
pub use timeout::Dir;

/// Sockets alive or awaiting `netd`'s acknowledgement of their close.
pub const MAX_SOCKETS: usize = 64;
/// Requests queued for `netd` at once.
pub const MAX_REQUESTS: usize = 256;
/// Connections a listener holds before `netd` is told to refuse more.
pub const ACCEPT_QUEUE: usize = 16;
/// Bytes of address in front of every datagram in a datagram socket's pair:
/// four octets of IPv4 address and a big-endian port.
pub const DGRAM_HEADER: usize = 6;
/// The longest datagram payload (IPv4 less the IP and UDP headers).
pub const MAX_DGRAM: usize = 1472;

/// Linux errno values this module reports (positive).
pub mod errno {
    pub const EBADF: i32 = 9;
    pub const EAGAIN: i32 = 11;
    pub const ENOMEM: i32 = 12;
    pub const EACCES: i32 = 13;
    pub const EBUSY: i32 = 16;
    pub const EINVAL: i32 = 22;
    pub const ENFILE: i32 = 23;
    pub const EPIPE: i32 = 32;
    pub const EADDRINUSE: i32 = 98;
    pub const ENOBUFS: i32 = 105;
    pub const EISCONN: i32 = 106;
    pub const ENOTCONN: i32 = 107;
    pub const ETIMEDOUT: i32 = 110;
    pub const EALREADY: i32 = 114;
    pub const EINPROGRESS: i32 = 115;
    pub const EINTR: i32 = 4;
    pub const ENETDOWN: i32 = 100;
    pub const EOPNOTSUPP: i32 = 95;
}

/// Stream (TCP) or datagram (UDP).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Stream,
    Dgram,
}

/// An IPv4 address and port.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Addr {
    pub ip: [u8; 4],
    pub port: u16,
}

impl Addr {
    pub const ANY: Addr = Addr {
        ip: [0; 4],
        port: 0,
    };
}

/// Where a socket is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Created; nothing asked of `netd` yet.
    Fresh,
    /// Bound to a local address and port.
    Bound,
    /// A `connect` is waiting for `netd` (the socket is non-blocking, or the
    /// caller was interrupted).
    Connecting,
    /// Has a data path.
    Connected,
    /// Accepting connections.
    Listening,
}

/// An operation `netd` is asked to carry out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Bind(Addr),
    Connect(Addr),
    Listen(u32),
    /// The last descriptor was closed: release the stack's socket.
    Close,
}

/// A queued request: the socket's id and what to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub id: u32,
    pub kind: Kind,
    pub op: Op,
}

/// What the table keeps for each socket.
struct Slot {
    id: u32,
    sock: Weak<InetSock>,
    /// `netd`'s side of the data path, once there is one.
    net_pair: Option<Arc<SocketPair>>,
    /// The last descriptor is gone; the slot lives until `netd` acknowledges.
    closed: bool,
}

struct Table {
    slots: Vec<Option<Slot>>,
    queue: VecDeque<Request>,
    /// The slot of the task serving the pump, if any.
    netd: Option<usize>,
    next_generation: u32,
}

static TABLE: Mutex<Table> = Mutex::new(Table {
    slots: Vec::new(),
    queue: VecDeque::new(),
    netd: None,
    next_generation: 1,
});

impl Table {
    fn slot_of(id: u32) -> usize {
        (id & 0xFF) as usize
    }

    fn get(&self, id: u32) -> Option<&Slot> {
        self.slots
            .get(Self::slot_of(id))?
            .as_ref()
            .filter(|slot| slot.id == id)
    }

    fn get_mut(&mut self, id: u32) -> Option<&mut Slot> {
        self.slots
            .get_mut(Self::slot_of(id))?
            .as_mut()
            .filter(|slot| slot.id == id)
    }

    fn live(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// Queue a request. A full queue (no `netd` is draining it) refuses a
    /// bind, connect or listen so its caller fails at once; a close is always
    /// queued, because the slot cannot be freed without it (and there is at
    /// most one per socket).
    fn push(&mut self, request: Request) -> bool {
        if request.op != Op::Close && self.queue.len() >= MAX_REQUESTS {
            return false;
        }
        self.queue.push_back(request);
        true
    }
}

/// What the in-kernel suite runs where a caller would sleep for `netd`: a
/// fake `netd` that answers the queued requests.
#[cfg(lazyos_tests)]
pub static RESPONDER: Mutex<Option<fn()>> = Mutex::new(None);

/// Make a socket of `kind`, or `None` at [`MAX_SOCKETS`].
pub fn create(kind: Kind) -> Option<Arc<InetSock>> {
    let mut table = TABLE.lock();
    if table.slots.is_empty() {
        table.slots.resize_with(MAX_SOCKETS, || None);
    }
    if table.live() >= MAX_SOCKETS {
        return None;
    }
    let index = table.slots.iter().position(Option::is_none)?;
    // The generation keeps a stale id from naming a newer socket.
    let id = (index as u32) | (table.next_generation << 8);
    table.next_generation = (table.next_generation.wrapping_add(1) & 0x00FF_FFFF).max(1);
    let sock = InetSock::new(id, kind);
    table.slots[index] = Some(Slot {
        id,
        sock: Arc::downgrade(&sock),
        net_pair: None,
        closed: false,
    });
    Some(sock)
}

/// Sockets in the table (alive, or closed and awaiting `netd`).
pub fn live_count() -> usize {
    TABLE.lock().live()
}

/// Requests waiting for `netd`.
pub fn queued_count() -> usize {
    TABLE.lock().queue.len()
}

/// Forget every socket and request (test isolation, and `netd` restarting).
pub fn reset() {
    let mut table = TABLE.lock();
    for slot in table.slots.iter_mut() {
        if let Some(Slot {
            net_pair: Some(pair),
            ..
        }) = slot.take()
        {
            pair.close(crate::ipc::pipe::Side::A);
        }
    }
    table.queue.clear();
    table.netd = None;
}
