//! `os.lazy.net.socket.v1` (`idl/net.midl`): request dispatch for sockets.
//!
//! Every call names a socket id and runs as the kernel-stamped sender (see
//! `owners.rs` for what "sender" means); the socket table refuses anyone but
//! the owner. A call that cannot finish is *parked*: `netd` keeps the
//! transaction and answers it from the event loop when the stack has made the
//! socket ready, or with `ETIMEDOUT` at the call's own deadline. One thread
//! serves any number of waiting callers.

use alloc::vec::Vec;
use core::cell::Cell;

use libmessenger::client::Errno;
use libmessenger::server::{fault_reply, Caller, Fault};
use netstack::stack::MAX_CHUNK;
use netstack::{Kind, SockAddr, SockError};
use user::messenger::netsock::{self as api, errno as sock_errno, wire};
use user::messenger::{errno, Error as MsgError, Parcel};
use user::sys;

use super::parked::{Done, Op};
use super::service::Netd;

pub(super) type Result<T> = core::result::Result<T, MsgError>;

/// Refusals written to the log before it goes quiet (they are all counted).
const DENY_LOG_LIMIT: u32 = 16;
/// Ticks between looks for owners that exited.
pub(super) const SWEEP_TICKS: u64 = 20;

pub(super) fn err(code: i64) -> MsgError {
    MsgError::Errno(-code)
}

/// The errno a socket-table failure is reported as.
pub(super) fn errno_of(error: SockError) -> i64 {
    match error {
        SockError::BadSocket => sock_errno::EBADF,
        SockError::NotOwner | SockError::Privileged => errno::EACCES,
        SockError::TooManyForOwner => sock_errno::EMFILE,
        SockError::TooMany => sock_errno::ENFILE,
        SockError::InvalidState | SockError::BadAddress => errno::EINVAL,
        SockError::AddrInUse => sock_errno::EADDRINUSE,
        SockError::Unreachable => sock_errno::ENETUNREACH,
        SockError::Refused => sock_errno::ECONNREFUSED,
        SockError::Reset => sock_errno::ECONNRESET,
        SockError::NotConnected => sock_errno::ENOTCONN,
        SockError::Pipe => errno::EPIPE,
        SockError::MessageSize => sock_errno::EMSGSIZE,
        SockError::WouldBlock => errno::EAGAIN,
    }
}

/// Counters `netd` keeps beyond the socket table's own.
#[derive(Default)]
pub(super) struct SockStats {
    pub(super) parked: u64,
    pub(super) park_timeouts: u64,
    not_owner: u64,
    denials_logged: u32,
    /// Set by the error mapping when a call named a socket of another owner.
    pub(super) owner_denied: Cell<bool>,
}

pub(super) fn error_parcel(method: u32, code: i64) -> Parcel {
    fault_reply(api::INTERFACE, method, &Fault::from_syscall(code))
}

pub(super) fn sock_addr(addr: &wire::SockAddr) -> Result<SockAddr> {
    let octets: [u8; 4] = addr
        .addr
        .as_slice()
        .try_into()
        .map_err(|_| err(errno::EINVAL))?;
    let port = u16::try_from(addr.port).map_err(|_| err(errno::EINVAL))?;
    Ok(SockAddr { addr: octets, port })
}

pub(super) fn wire_addr(addr: SockAddr) -> wire::SockAddr {
    wire::SockAddr {
        addr: addr.addr.to_vec(),
        port: u32::from(addr.port),
    }
}

/// `timeout_ms` 0 is the longest wait; anything else must be 10 to 60000.
pub(super) fn wait_ms(timeout_ms: u32) -> Result<i64> {
    match timeout_ms {
        0 => Ok(i64::from(api::MAX_TIMEOUT_MS)),
        10..=api::MAX_TIMEOUT_MS => Ok(i64::from(timeout_ms)),
        _ => Err(err(errno::EINVAL)),
    }
}

impl Netd {
    /// The owner the kernel-stamped sender stands for, or `EACCES`.
    fn owner(&mut self, cx: &Caller) -> core::result::Result<u64, Fault> {
        let owner = self
            .tasks
            .owner_of(cx.sender, sys::clock())
            .ok_or_else(|| Fault::errno(Errno::ACCES))?;
        self.call.owner = owner;
        Ok(owner)
    }

    /// Count (and, a few times, log) a refusal: every one is audited.
    pub(super) fn note_denied(&mut self, owner: u64, method: u32) {
        self.sock_stats.not_owner += 1;
        if self.sock_stats.denials_logged < DENY_LOG_LIMIT {
            self.sock_stats.denials_logged += 1;
            sys::write_str(&alloc::format!(
                "NETD:DENY owner={owner:#x} method={method}\n"
            ));
        }
    }

    /// The errno for a socket-table failure; a call on another owner's socket
    /// is remembered so the dispatcher can audit it.
    pub(super) fn sock_error(&self, error: SockError) -> MsgError {
        if error == SockError::NotOwner {
            self.sock_stats.owner_denied.set(true);
        }
        err(errno_of(error))
    }

    fn socket_stats(&self) -> wire::SocketStats {
        let c = self.net.socket_counters();
        wire::SocketStats {
            open: self.net.socket_open_count() as u32,
            opened: c.opened,
            closed: c.closed,
            reclaimed: c.reclaimed,
            connected: c.connected,
            accepted: c.accepted,
            refused: c.refused,
            resets: c.resets,
            tx_bytes: c.tx_bytes,
            rx_bytes: c.rx_bytes,
            tx_datagrams: c.tx_datagrams,
            rx_datagrams: c.rx_datagrams,
            parked: self.sock_stats.parked,
            park_timeouts: self.sock_stats.park_timeouts,
            not_owner: self.sock_stats.not_owner,
        }
    }
}

type Reply<T> = core::result::Result<T, Fault>;

impl wire::Server for Netd {
    fn open(&mut self, cx: &Caller, kind: u32) -> Reply<u32> {
        let owner = self.owner(cx)?;
        let kind = match kind {
            wire::SOCK_KIND_STREAM => Kind::Stream,
            wire::SOCK_KIND_DATAGRAM => Kind::Datagram,
            _ => return Err(Fault::errno(Errno::INVAL)),
        };
        Ok(self
            .net
            .socket_open(owner, kind)
            .map_err(|e| self.sock_error(e))?)
    }

    fn bind(&mut self, cx: &Caller, sock: u32, addr: wire::SockAddr) -> Reply<()> {
        let owner = self.owner(cx)?;
        let local = sock_addr(&addr)?;
        Ok(self
            .net
            .socket_bind(sock, owner, local)
            .map_err(|e| self.sock_error(e))?)
    }

    fn connect(
        &mut self,
        cx: &Caller,
        sock: u32,
        addr: wire::SockAddr,
        timeout_ms: u32,
    ) -> Reply<()> {
        let owner = self.owner(cx)?;
        let wait = wait_ms(timeout_ms)?;
        let peer = sock_addr(&addr)?;
        // A call that timed out left the handshake running: asking again
        // waits for the same attempt instead of starting one.
        match self.net.socket_connect_status(sock, owner) {
            Ok(false) => {}
            Err(error) if error != SockError::NotConnected => {
                return Err(self.sock_error(error).into())
            }
            _ => self
                .net
                .socket_connect(sock, owner, peer)
                .map_err(|e| self.sock_error(e))?,
        }
        match self.park(wire::METHOD_CONNECT, owner, sock, Op::Connect, wait)? {
            Done::Unit => Ok(()),
            _ => Err(Fault::unserved()),
        }
    }

    fn listen(&mut self, cx: &Caller, sock: u32, backlog: u32) -> Reply<()> {
        let owner = self.owner(cx)?;
        Ok(self
            .net
            .socket_listen(sock, owner, backlog)
            .map_err(|e| self.sock_error(e))?)
    }

    fn accept(&mut self, cx: &Caller, sock: u32, timeout_ms: u32) -> Reply<wire::AcceptReply> {
        let owner = self.owner(cx)?;
        let wait = wait_ms(timeout_ms)?;
        match self.park(wire::METHOD_ACCEPT, owner, sock, Op::Accept, wait)? {
            Done::Accept(reply) => Ok(reply),
            _ => Err(Fault::unserved()),
        }
    }

    fn send(&mut self, cx: &Caller, sock: u32, data: Vec<u8>, timeout_ms: u32) -> Reply<u32> {
        let owner = self.owner(cx)?;
        let wait = wait_ms(timeout_ms)?;
        if data.is_empty() || data.len() > MAX_CHUNK {
            return Err(Fault::errno(Errno::INVAL));
        }
        match self.park(wire::METHOD_SEND, owner, sock, Op::Send(data), wait)? {
            Done::Send(sent) => Ok(sent),
            _ => Err(Fault::unserved()),
        }
    }

    fn recv(&mut self, cx: &Caller, sock: u32, max: u32, timeout_ms: u32) -> Reply<Vec<u8>> {
        let owner = self.owner(cx)?;
        let wait = wait_ms(timeout_ms)?;
        match self.park(wire::METHOD_RECV, owner, sock, Op::Recv(max as usize), wait)? {
            Done::Recv(data) => Ok(data),
            _ => Err(Fault::unserved()),
        }
    }

    fn send_to(
        &mut self,
        cx: &Caller,
        sock: u32,
        addr: wire::SockAddr,
        data: Vec<u8>,
    ) -> Reply<u32> {
        let owner = self.owner(cx)?;
        let to = sock_addr(&addr)?;
        let sent = self
            .net
            .socket_sendto(sock, owner, to, &data)
            .map_err(|e| self.sock_error(e))?;
        Ok(sent as u32)
    }

    fn recv_from(
        &mut self,
        cx: &Caller,
        sock: u32,
        max: u32,
        timeout_ms: u32,
    ) -> Reply<wire::RecvFromReply> {
        let owner = self.owner(cx)?;
        let wait = wait_ms(timeout_ms)?;
        match self.park(
            wire::METHOD_RECVFROM,
            owner,
            sock,
            Op::RecvFrom(max as usize),
            wait,
        )? {
            Done::RecvFrom(reply) => Ok(reply),
            _ => Err(Fault::unserved()),
        }
    }

    fn poll(&mut self, cx: &Caller, sock: u32, interest: u32, timeout_ms: u32) -> Reply<u32> {
        let owner = self.owner(cx)?;
        let wait = wait_ms(timeout_ms)?;
        if interest == 0 || interest > 0x1F {
            return Err(Fault::errno(Errno::INVAL));
        }
        match self.park(wire::METHOD_POLL, owner, sock, Op::Poll(interest), wait)? {
            Done::Poll(ready) => Ok(ready),
            _ => Err(Fault::unserved()),
        }
    }

    fn shutdown(&mut self, cx: &Caller, sock: u32, how: u32) -> Reply<()> {
        let owner = self.owner(cx)?;
        let (read, write) = match how {
            wire::SHUTDOWN_READ => (true, false),
            wire::SHUTDOWN_WRITE => (false, true),
            wire::SHUTDOWN_BOTH => (true, true),
            _ => return Err(Fault::errno(Errno::INVAL)),
        };
        Ok(self
            .net
            .socket_shutdown(sock, owner, read, write)
            .map_err(|e| self.sock_error(e))?)
    }

    fn local_addr(&mut self, cx: &Caller, sock: u32) -> Reply<wire::SockAddr> {
        let owner = self.owner(cx)?;
        let addr = self
            .net
            .socket_local_addr(sock, owner)
            .map_err(|e| self.sock_error(e))?;
        Ok(wire_addr(addr))
    }

    fn peer_addr(&mut self, cx: &Caller, sock: u32) -> Reply<wire::SockAddr> {
        let owner = self.owner(cx)?;
        let addr = self
            .net
            .socket_peer_addr(sock, owner)
            .map_err(|e| self.sock_error(e))?;
        Ok(wire_addr(addr))
    }

    fn close(&mut self, cx: &Caller, sock: u32) -> Reply<()> {
        let owner = self.owner(cx)?;
        self.net
            .socket_close(sock, owner, self.call.now_ms)
            .map_err(|e| self.sock_error(e))?;
        self.drop_calls_on(owner, sock);
        Ok(())
    }

    fn stats(&mut self, cx: &Caller) -> Reply<wire::SocketStats> {
        self.owner(cx)?;
        Ok(self.socket_stats())
    }
}
