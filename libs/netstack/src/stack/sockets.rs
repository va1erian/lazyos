//! The socket table (docs/networking-plan.md N3): who owns which socket, the
//! quotas, port allocation, closing and reclaim. The stream operations are in
//! `tcp.rs`, the datagram ones in `udp.rs`; both go through [`Sockets::entry`]
//! so every call checks the id and the owner before it touches a socket.
//!
//! **Bounded.** At most [`MAX_SOCKETS`] sockets, [`MAX_PER_OWNER`] per owner,
//! fixed buffers per socket, and at most [`MAX_CLOSING`] streams finishing
//! their close handshake in the background; nothing is sized by a client.
//!
//! **Ids do not come back quickly.** A socket id carries a generation above
//! its slot number, so a stale id from a closed socket is `BadSocket`, not
//! somebody else's new socket.

use alloc::vec;
use alloc::vec::Vec;

use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::socket::{tcp, udp};
use smoltcp::time::Duration;

mod closing;

/// Sockets open at once, all owners together.
pub const MAX_SOCKETS: usize = 64;
/// Sockets one owner may hold.
pub const MAX_PER_OWNER: usize = 8;
/// Connections a listener may have completed and waiting, at most.
pub const MAX_BACKLOG: usize = 8;
/// Bytes of buffer each way for a stream socket. 256 KiB windows (smoltcp
/// scales the advertised window from the receive buffer: shift 2) keep a
/// bulk transfer from stalling on the window between round trips
/// (docs/performance-plan.md P4.3); 64 sockets cost 32 MiB at most.
pub const TCP_BUFFER: usize = 256 * 1024;
/// How long a received segment may wait for its ACK when no second one
/// follows (smoltcp's default too; said here so the clock it rounds to,
/// `netd`'s 10 ms tick, is visible next to it).
pub const ACK_DELAY_MS: u64 = 10;
/// Largest `Send` and `Recv` chunk.
pub const MAX_CHUNK: usize = 16 * 1024;
/// Datagrams a datagram socket queues each way.
pub const UDP_DATAGRAMS: usize = 8;
/// Largest datagram payload (an MTU of 1500 less the IP and UDP headers).
pub const UDP_PAYLOAD: usize = 1472;
/// First ephemeral port.
pub const EPHEMERAL_FIRST: u16 = 49152;
/// Ports below this need a capability `netd` cannot check yet: refused.
pub const PRIVILEGED_PORTS: u16 = 1024;
/// Closed streams still finishing their handshake, at most; the oldest is
/// aborted past this.
pub const MAX_CLOSING: usize = 32;
/// Milliseconds a closing stream may linger before it is aborted.
pub const CLOSING_MS: i64 = 30_000;
/// A connection attempt or an idle retransmission gives up after this.
const TCP_TIMEOUT_SECS: u64 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Stream,
    Datagram,
}

/// An IPv4 address and port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SockAddr {
    pub addr: [u8; 4],
    pub port: u16,
}

impl SockAddr {
    pub const ANY: SockAddr = SockAddr {
        addr: [0; 4],
        port: 0,
    };
}

/// Why a socket operation failed. `netd` maps each to an errno.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SockError {
    /// No socket has this id.
    BadSocket,
    /// The socket belongs to another caller.
    NotOwner,
    /// The caller holds [`MAX_PER_OWNER`] sockets.
    TooManyForOwner,
    /// [`MAX_SOCKETS`] are open.
    TooMany,
    /// The call does not fit the socket's kind or state.
    InvalidState,
    /// An address or length argument is not acceptable.
    BadAddress,
    /// The port is held by another socket of the same kind.
    AddrInUse,
    /// A port below [`PRIVILEGED_PORTS`].
    Privileged,
    /// No address, or no route to the destination.
    Unreachable,
    /// The peer reset the connection attempt.
    Refused,
    /// The peer reset an open connection.
    Reset,
    /// Stream I/O before a connection exists.
    NotConnected,
    /// Writing after the stream was shut for writing.
    Pipe,
    /// A datagram that does not fit.
    MessageSize,
    /// Nothing can be done right now (the caller parks or retries).
    WouldBlock,
}

/// Readiness bits, in the order of the `Ready` enum of `os.lazy.net.socket.v1`.
pub mod ready {
    pub const READABLE: u32 = 1 << 0;
    pub const WRITABLE: u32 = 1 << 1;
    pub const ACCEPTABLE: u32 = 1 << 2;
    pub const CLOSED: u32 = 1 << 3;
    pub const ERROR: u32 = 1 << 4;
}

/// Counters over the life of the table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SocketCounters {
    pub opened: u64,
    pub closed: u64,
    pub reclaimed: u64,
    pub connected: u64,
    pub accepted: u64,
    pub refused: u64,
    pub resets: u64,
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub tx_datagrams: u64,
    pub rx_datagrams: u64,
}

/// What a stream socket has been through, as observed once per poll.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct StreamState {
    /// The port `Bind` chose, before `Connect` or `Listen` uses it.
    pub bound_port: Option<u16>,
    pub connecting: bool,
    pub established: bool,
    /// The peer's FIN was seen (or the stream ended in order).
    pub fin_seen: bool,
    /// The connection attempt ended in a reset.
    pub refused: bool,
    /// An established connection ended in a reset.
    pub reset: bool,
    pub shut_read: bool,
    pub shut_write: bool,
}

pub(super) enum Inner {
    Tcp {
        handle: SocketHandle,
        state: StreamState,
    },
    Listener {
        port: u16,
        backlog: Vec<SocketHandle>,
    },
    Udp {
        handle: SocketHandle,
        peer: Option<SockAddr>,
    },
}

pub(super) struct Entry {
    pub id: u32,
    pub owner: u64,
    pub inner: Inner,
}

impl Entry {
    pub fn kind(&self) -> Kind {
        match self.inner {
            Inner::Udp { .. } => Kind::Datagram,
            _ => Kind::Stream,
        }
    }
}

pub struct Sockets {
    slots: Vec<Option<Entry>>,
    generation: u32,
    next_ephemeral: u16,
    /// Streams whose owner closed them, still finishing on the wire, with
    /// the stack time they were closed at.
    closing: Vec<(SocketHandle, i64)>,
    pub(super) counters: SocketCounters,
}

pub(super) fn new_tcp_socket() -> tcp::Socket<'static> {
    let mut socket = tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0u8; TCP_BUFFER]),
        tcp::SocketBuffer::new(vec![0u8; TCP_BUFFER]),
    );
    socket.set_timeout(Some(Duration::from_secs(TCP_TIMEOUT_SECS)));
    // Interactive tools send small writes; do not hold them for an ACK.
    socket.set_nagle_enabled(false);
    socket.set_ack_delay(Some(Duration::from_millis(ACK_DELAY_MS)));
    // Without a controller smoltcp sends a whole window at once, whatever
    // the path drops; Reno backs off on loss (Cubic would need f64 here).
    socket.set_congestion_control(tcp::CongestionControl::Reno);
    socket
}

fn new_udp_socket() -> udp::Socket<'static> {
    udp::Socket::new(
        udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY; UDP_DATAGRAMS],
            vec![0u8; UDP_DATAGRAMS * UDP_PAYLOAD],
        ),
        udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY; UDP_DATAGRAMS],
            vec![0u8; UDP_DATAGRAMS * UDP_PAYLOAD],
        ),
    )
}

impl Sockets {
    pub(super) fn new(seed: u64) -> Sockets {
        Sockets {
            slots: (0..MAX_SOCKETS).map(|_| None).collect(),
            generation: 1,
            // Start the ephemeral range at a seeded offset, so a restart does
            // not reuse the ports of the connections it just lost.
            next_ephemeral: EPHEMERAL_FIRST + (seed as u16 % (u16::MAX - EPHEMERAL_FIRST)),
            closing: Vec::new(),
            counters: SocketCounters::default(),
        }
    }

    pub fn counters(&self) -> &SocketCounters {
        &self.counters
    }

    /// Sockets open right now.
    pub fn open_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// Sockets `owner` holds.
    pub fn owned_by(&self, owner: u64) -> usize {
        self.slots
            .iter()
            .flatten()
            .filter(|e| e.owner == owner)
            .count()
    }

    /// Every owner of an open socket, each once.
    pub fn owners(&self) -> Vec<u64> {
        let mut owners: Vec<u64> = self.slots.iter().flatten().map(|e| e.owner).collect();
        owners.sort_unstable();
        owners.dedup();
        owners
    }

    /// Streams closed by their owner and still finishing on the wire.
    pub fn closing_count(&self) -> usize {
        self.closing.len()
    }

    /// Every open socket id, for the fuzzer and the tests.
    pub fn ids(&self) -> Vec<u32> {
        self.slots.iter().flatten().map(|e| e.id).collect()
    }

    fn slot_of(id: u32) -> usize {
        (id & 0xFF) as usize
    }

    /// The entry `id` names, if `owner` owns it.
    pub(super) fn entry(&mut self, id: u32, owner: u64) -> Result<&mut Entry, SockError> {
        let slot = Sockets::slot_of(id);
        let entry = self
            .slots
            .get_mut(slot)
            .and_then(|s| s.as_mut())
            .filter(|e| e.id == id)
            .ok_or(SockError::BadSocket)?;
        if entry.owner != owner {
            return Err(SockError::NotOwner);
        }
        Ok(entry)
    }

    /// Whether a socket `owner` could open now would be refused, and why.
    pub(super) fn check_quota(&self, owner: u64) -> Result<(), SockError> {
        if self.owned_by(owner) >= MAX_PER_OWNER {
            return Err(SockError::TooManyForOwner);
        }
        if self.open_count() >= MAX_SOCKETS {
            return Err(SockError::TooMany);
        }
        Ok(())
    }

    /// Put `inner` in a free slot for `owner`; the new id. The caller has
    /// passed [`Sockets::check_quota`], so a slot is free.
    pub(super) fn insert(&mut self, owner: u64, inner: Inner) -> u32 {
        let slot = self
            .slots
            .iter()
            .position(|s| s.is_none())
            .expect("check_quota passed: a slot is free");
        let id = (slot as u32) | (self.generation << 8);
        // The generation never reaches zero again: a wrap skips it.
        self.generation = (self.generation.wrapping_add(1) & 0x00FF_FFFF).max(1);
        self.slots[slot] = Some(Entry { id, owner, inner });
        self.counters.opened += 1;
        id
    }

    /// Make a socket of `kind` for `owner`.
    pub(super) fn open(
        &mut self,
        sockets: &mut SocketSet<'static>,
        owner: u64,
        kind: Kind,
    ) -> Result<u32, SockError> {
        self.check_quota(owner)?;
        let inner = match kind {
            Kind::Stream => Inner::Tcp {
                handle: sockets.add(new_tcp_socket()),
                state: StreamState::default(),
            },
            Kind::Datagram => Inner::Udp {
                handle: sockets.add(new_udp_socket()),
                peer: None,
            },
        };
        Ok(self.insert(owner, inner))
    }

    /// Whether `port` is held by a socket of `kind` (including a stream that
    /// is still closing, whose port the wire still knows).
    pub(super) fn port_in_use(&self, sockets: &SocketSet<'static>, kind: Kind, port: u16) -> bool {
        let held = self.slots.iter().flatten().any(|e| match &e.inner {
            Inner::Tcp { handle, state } if kind == Kind::Stream => {
                state.bound_port == Some(port)
                    || sockets
                        .get::<tcp::Socket>(*handle)
                        .local_endpoint()
                        .is_some_and(|ep| ep.port == port)
            }
            Inner::Listener { port: p, .. } if kind == Kind::Stream => *p == port,
            Inner::Udp { handle, .. } if kind == Kind::Datagram => {
                sockets.get::<udp::Socket>(*handle).endpoint().port == port
            }
            _ => false,
        });
        held || (kind == Kind::Stream
            && self.closing.iter().any(|(h, _)| {
                sockets
                    .get::<tcp::Socket>(*h)
                    .local_endpoint()
                    .is_some_and(|ep| ep.port == port)
            }))
    }

    /// An ephemeral port no socket of `kind` holds.
    pub(super) fn pick_port(&mut self, sockets: &SocketSet<'static>, kind: Kind) -> Option<u16> {
        for _ in 0..(u16::MAX - EPHEMERAL_FIRST) {
            let port = self.next_ephemeral;
            self.next_ephemeral = if port == u16::MAX {
                EPHEMERAL_FIRST
            } else {
                port + 1
            };
            if !self.port_in_use(sockets, kind, port) {
                return Some(port);
            }
        }
        None
    }

    /// Check a port a client asked for: `Ok(None)` means "pick one".
    pub(super) fn check_port(
        &self,
        sockets: &SocketSet<'static>,
        kind: Kind,
        port: u16,
    ) -> Result<Option<u16>, SockError> {
        if port == 0 {
            return Ok(None);
        }
        if port < PRIVILEGED_PORTS {
            return Err(SockError::Privileged);
        }
        if self.port_in_use(sockets, kind, port) {
            return Err(SockError::AddrInUse);
        }
        Ok(Some(port))
    }

    /// Every stream entry, for the per-poll state observation.
    pub(super) fn streams_mut(&mut self) -> impl Iterator<Item = (&mut StreamState, SocketHandle)> {
        self.slots
            .iter_mut()
            .flatten()
            .filter_map(|e| match &mut e.inner {
                Inner::Tcp { handle, state } => Some((state, *handle)),
                _ => None,
            })
    }
}
