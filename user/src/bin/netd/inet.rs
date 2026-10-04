//! The pump for Linux programs' sockets (`docs/networking-plan.md`, stage N5).
//!
//! The kernel owns the application's side of every `AF_INET` socket
//! (`kernel/src/ipc/inet`); `netd` owns the other. The event loop calls
//! [`Inet::pump`] on every pass, and the kernel's doorbell for these sockets
//! (`kernel/src/ipc/inet/bell.rs`) wakes it whenever an application queued a
//! request, wrote into an empty ring, made room in a full one or closed.
//! Each pass does three things:
//!
//! 1. fetches the requests the kernel queued (bind, connect, listen, close)
//!    and carries them out against the stack;
//! 2. moves bytes both ways between each socket's kernel data path and its
//!    stack socket, a chunk at a time, never waiting: a full buffer on either
//!    side just means "next pass";
//! 3. notices what the network did (a connection finished, arrived, ended or
//!    failed) and tells the kernel.
//!
//! Every stack socket has an owner of its own (`OWNER_BASE | kernel id`), so
//! the per-owner quota of the Messenger sockets never limits a busy server.
//! A socket the kernel closes is closed gracefully after the bytes the
//! application wrote have been sent, and acknowledged so the kernel can free
//! its slot.

use alloc::vec::Vec;

use netstack::{Kind, SockAddr, SockError, Stack};
use user::sys::{self, INET_ADDR_BLOCK, INET_REQUEST_BYTES};

use super::sock::errno_of;

#[path = "inet/flow.rs"]
mod flow;

/// Stack owners of kernel sockets: far above any Messenger owner id.
pub(super) const OWNER_BASE: u64 = 1 << 40;
/// Bytes moved per read or write.
const CHUNK: usize = 16 * 1024;
/// Largest datagram message: a 6-byte address header and 1472 bytes.
const FRAME: usize = 6 + 1472;
/// Requests handled per pass, so a flood cannot hold the loop.
const REQUESTS_PER_PASS: usize = 64;
/// A closing stream that cannot flush within this long is aborted, ms.
const CLOSE_LINGER_MS: i64 = 30_000;
/// Ticks between attach attempts while the kernel refuses (not root/`_netd`).
const ATTACH_RETRY_TICKS: u64 = 500;

const ECONNRESET: i32 = 104;
const ECONNREFUSED: i32 = 111;
const ENETUNREACH: i32 = 101;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Made, possibly bound; not connected or listening.
    Fresh,
    /// A `connect` is in flight.
    Connecting,
    /// Moving data (a datagram socket is always here once bound).
    Established,
    Listening,
    /// The kernel closed the socket; flushing before the stack closes it.
    Closing(i64),
}

struct Entry {
    id: u32,
    kind: Kind,
    stack: Option<u32>,
    phase: Phase,
    /// Bytes read from the application, not yet taken by the stack (a stream
    /// chunk, or one datagram message).
    tx: Vec<u8>,
    /// Bytes read from the stack, not yet taken by the application.
    rx: Vec<u8>,
    /// The application finished sending.
    app_end: bool,
    /// The stack's send side was shut after the application's end.
    shut: bool,
    /// The stack finished (the peer's FIN, or an error): tell the kernel once
    /// `rx` has been delivered.
    net_end: bool,
    net_end_told: bool,
}

impl Entry {
    fn new(id: u32, kind: Kind) -> Entry {
        Entry {
            id,
            kind,
            stack: None,
            phase: Phase::Fresh,
            tx: Vec::new(),
            rx: Vec::new(),
            app_end: false,
            shut: false,
            net_end: false,
            net_end_told: false,
        }
    }

    fn owner(&self) -> u64 {
        OWNER_BASE | u64::from(self.id)
    }
}

/// Counters over the life of the pump, for `NETD:INET` lines and `netctl`.
#[derive(Default)]
pub(super) struct InetStats {
    pub connects: u64,
    pub accepts: u64,
    pub binds: u64,
    pub closed: u64,
    pub to_stack: u64,
    pub from_stack: u64,
    /// Bytes handed to applications and taken from them.
    pub to_app: u64,
    pub from_app: u64,
}

impl InetStats {
    /// Every byte counter added up: unchanged across a pass means nothing moved.
    fn moved(&self) -> u64 {
        self.to_stack + self.from_stack + self.to_app + self.from_app
    }
}

pub(super) struct Inet {
    entries: Vec<Entry>,
    attached: bool,
    next_attach: u64,
    pub(super) stats: InetStats,
}

/// `local` and `peer` in the kernel's address-block form.
fn block(local: Option<SockAddr>, peer: Option<SockAddr>) -> [u8; INET_ADDR_BLOCK] {
    let mut out = [0u8; INET_ADDR_BLOCK];
    for (at, addr) in [(0, local), (6, peer)] {
        if let Some(a) = addr {
            out[at..at + 4].copy_from_slice(&a.addr);
            out[at + 4..at + 6].copy_from_slice(&a.port.to_le_bytes());
        }
    }
    out
}

/// The errno for a failed connection or send.
fn net_errno(error: SockError) -> i32 {
    match error {
        SockError::Refused => ECONNREFUSED,
        SockError::Reset => ECONNRESET,
        SockError::Unreachable => ENETUNREACH,
        other => errno_of(other) as i32,
    }
}

impl Inet {
    pub(super) fn new() -> Inet {
        Inet {
            entries: Vec::new(),
            attached: false,
            next_attach: 0,
            stats: InetStats::default(),
        }
    }

    /// Whether this task serves the kernel's sockets (and may park on their
    /// doorbell).
    pub(super) fn attached(&self) -> bool {
        self.attached
    }

    fn entry(&mut self, id: u32, kind: Kind) -> &mut Entry {
        if let Some(at) = self.entries.iter().position(|e| e.id == id) {
            return &mut self.entries[at];
        }
        self.entries.push(Entry::new(id, kind));
        self.entries.last_mut().expect("just pushed")
    }

    /// One pass. `tick` is the kernel tick, `now_ms` the stack clock.
    /// `true` when bytes moved: the caller passes again at once, because
    /// what is left (a stack buffer with more to read, a ring with more
    /// to send) rings no doorbell and may bring no frame.
    pub(super) fn pump(&mut self, stack: &mut Stack, tick: u64, now_ms: i64) -> bool {
        if !self.attached {
            if tick < self.next_attach {
                return false;
            }
            match sys::inet_attach() {
                Ok(()) => {
                    self.attached = true;
                    self.entries.clear();
                    sys::write_str("NETD:INET:ATTACHED\n");
                }
                Err(_) => {
                    // Not root or `_netd`, or the kernel is without the pump.
                    self.next_attach = tick + ATTACH_RETRY_TICKS;
                    return false;
                }
            }
        }
        for _ in 0..REQUESTS_PER_PASS {
            let mut raw = [0u8; INET_REQUEST_BYTES];
            match sys::inet_next(&mut raw) {
                Ok(true) => self.request(stack, &raw, now_ms),
                _ => break,
            }
        }
        let before = self.stats.moved();
        let mut i = 0;
        while i < self.entries.len() {
            let finished = self.service(stack, i, now_ms);
            if finished {
                self.entries.remove(i);
            } else {
                i += 1;
            }
        }
        self.stats.moved() != before
    }

    fn request(&mut self, stack: &mut Stack, raw: &[u8; INET_REQUEST_BYTES], now_ms: i64) {
        let code = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        let id = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]);
        let addr = SockAddr {
            addr: [raw[8], raw[9], raw[10], raw[11]],
            port: u16::from_le_bytes([raw[12], raw[13]]),
        };
        let kind = if raw[14] == 1 {
            Kind::Datagram
        } else {
            Kind::Stream
        };
        let backlog = u32::from_le_bytes([raw[16], raw[17], raw[18], raw[19]]);
        match code {
            1 => self.bind(stack, id, kind, addr),
            2 => self.connect(stack, id, kind, addr),
            3 => self.listen(stack, id, kind, backlog),
            4 => self.close(stack, id, now_ms),
            _ => {}
        }
    }

    /// The stack socket for `id`, made if it does not exist.
    fn ensure(&mut self, stack: &mut Stack, id: u32, kind: Kind) -> Result<(u32, u64), SockError> {
        let entry = self.entry(id, kind);
        let owner = entry.owner();
        if let Some(sid) = entry.stack {
            return Ok((sid, owner));
        }
        let sid = stack.socket_open(owner, kind)?;
        entry.stack = Some(sid);
        Ok((sid, owner))
    }

    fn answer(id: u32, status: i32, local: Option<SockAddr>, peer: Option<SockAddr>) {
        let _ = sys::inet_complete(id, status, &block(local, peer));
    }

    fn bind(&mut self, stack: &mut Stack, id: u32, kind: Kind, addr: SockAddr) {
        let result = self
            .ensure(stack, id, kind)
            .and_then(|(sid, owner)| stack.socket_bind(sid, owner, addr).map(|()| (sid, owner)));
        match result {
            Ok((sid, owner)) => {
                self.stats.binds += 1;
                if kind == Kind::Datagram {
                    self.entry(id, kind).phase = Phase::Established;
                }
                Inet::answer(id, 0, stack.socket_local_addr(sid, owner).ok(), None);
            }
            Err(e) => Inet::answer(id, errno_of(e) as i32, None, None),
        }
    }

    fn connect(&mut self, stack: &mut Stack, id: u32, kind: Kind, peer: SockAddr) {
        let result = self
            .ensure(stack, id, kind)
            .and_then(|(sid, owner)| stack.socket_connect(sid, owner, peer));
        match result {
            Ok(()) => self.entry(id, kind).phase = Phase::Connecting,
            Err(e) => {
                self.forget_stack(stack, id);
                Inet::answer(id, net_errno(e), None, None);
            }
        }
    }

    fn listen(&mut self, stack: &mut Stack, id: u32, kind: Kind, backlog: u32) {
        let result = self.ensure(stack, id, kind).and_then(|(sid, owner)| {
            stack.socket_listen(sid, owner, backlog)?;
            Ok((sid, owner))
        });
        match result {
            Ok((sid, owner)) => {
                self.entry(id, kind).phase = Phase::Listening;
                Inet::answer(id, 0, stack.socket_local_addr(sid, owner).ok(), None);
            }
            Err(e) => Inet::answer(id, errno_of(e) as i32, None, None),
        }
    }

    /// Drop a socket the stack could not use again (a refused connection) so
    /// the application's next `connect` starts clean.
    fn forget_stack(&mut self, stack: &mut Stack, id: u32) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            if let Some(sid) = entry.stack.take() {
                let _ = stack.socket_close(sid, entry.owner(), 0);
            }
            entry.phase = Phase::Fresh;
        }
    }

    fn close(&mut self, stack: &mut Stack, id: u32, now_ms: i64) {
        self.stats.closed += 1;
        let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) else {
            // Never reached the stack: nothing to release.
            let _ = sys::inet_close_ack(id);
            return;
        };
        if entry.stack.is_none() || matches!(entry.phase, Phase::Listening) {
            if let Some(sid) = entry.stack.take() {
                let _ = stack.socket_close(sid, entry.owner(), now_ms);
            }
            entry.phase = Phase::Closing(i64::MIN);
            return;
        }
        entry.phase = Phase::Closing(now_ms);
    }

    /// One socket's pass; `true` when it is finished and can be forgotten.
    fn service(&mut self, stack: &mut Stack, at: usize, now_ms: i64) -> bool {
        let phase = self.entries[at].phase;
        match phase {
            Phase::Closing(since) => self.service_closing(stack, at, since, now_ms),
            Phase::Connecting => self.service_connecting(stack, at),
            Phase::Listening => self.service_listener(stack, at),
            Phase::Established => {
                if self.entries[at].kind == Kind::Stream {
                    self.service_stream(stack, at);
                } else {
                    self.service_datagrams(stack, at);
                }
                false
            }
            Phase::Fresh => false,
        }
    }

    fn service_connecting(&mut self, stack: &mut Stack, at: usize) -> bool {
        let (id, sid, owner) = {
            let e = &self.entries[at];
            (e.id, e.stack.unwrap_or(0), e.owner())
        };
        match stack.socket_connect_status(sid, owner) {
            Ok(true) => {
                self.stats.connects += 1;
                self.entries[at].phase = Phase::Established;
                Inet::answer(
                    id,
                    0,
                    stack.socket_local_addr(sid, owner).ok(),
                    stack.socket_peer_addr(sid, owner).ok(),
                );
            }
            Ok(false) => {}
            Err(e) => {
                Inet::answer(id, net_errno(e), None, None);
                self.forget_stack(stack, id);
            }
        }
        false
    }

    fn service_listener(&mut self, stack: &mut Stack, at: usize) -> bool {
        let (id, sid, owner) = {
            let e = &self.entries[at];
            (e.id, e.stack.unwrap_or(0), e.owner())
        };
        // A few per pass: a SYN flood fills the backlog, not this loop.
        for _ in 0..4 {
            // The accepted socket gets an owner of its own once the kernel
            // has named it; until then a placeholder keeps it off the listener's.
            let placeholder = OWNER_BASE | (1 << 39);
            let Ok(Some((conn, peer))) = stack.socket_accept_as(sid, owner, placeholder) else {
                break;
            };
            let local = stack.socket_local_addr(conn, placeholder).ok();
            match sys::inet_accepted(id, &block(local, Some(peer))) {
                Ok(new_id) => {
                    self.stats.accepts += 1;
                    let mut entry = Entry::new(new_id, Kind::Stream);
                    // Hand the socket to its own owner so its quota is its own.
                    // If that fails the placeholder still owns it: release it
                    // and tell the kernel, rather than strand a connection no
                    // later call could reach.
                    if stack
                        .socket_chown(conn, placeholder, entry.owner())
                        .is_err()
                    {
                        let _ = stack.socket_close(conn, placeholder, 0);
                        let _ = sys::inet_error(new_id, ECONNRESET);
                        let _ = sys::inet_eof(new_id);
                        continue;
                    }
                    entry.stack = Some(conn);
                    entry.phase = Phase::Established;
                    self.entries.push(entry);
                }
                // The kernel's queue is full or the table is: refuse the connection.
                Err(_) => {
                    let _ = stack.socket_close(conn, placeholder, 0);
                }
            }
        }
        false
    }
}
