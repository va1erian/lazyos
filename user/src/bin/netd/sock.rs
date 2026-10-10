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

use netstack::stack::MAX_CHUNK;
use netstack::{Kind, SockAddr, SockError};
use user::messenger::netsock::{self as api, errno as sock_errno, wire};
use user::messenger::{errno, services, Error as MsgError, Message, Parcel};
use user::sys;

use super::parked::Op;
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
    owner_denied: Cell<bool>,
}

pub(super) fn reply(
    method: u32,
    body: core::result::Result<Vec<u8>, libmessenger::Error>,
) -> Result<Parcel> {
    body.map(|body| api::parcel(method, body))
        .map_err(MsgError::Parcel)
}

pub(super) fn error_parcel(method: u32, code: i64) -> Parcel {
    services::error_reply(api::INTERFACE, method, err(code))
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
    /// Route one socket request. `Ok(Some)` is the reply; `Ok(None)` means
    /// the call was parked and is answered later.
    pub(super) fn dispatch_socket(
        &mut self,
        message: &Message,
        now_ms: i64,
    ) -> Result<Option<Parcel>> {
        let tick = sys::clock();
        let method = message.method();
        let body = &message.parcel.body;
        let Some(owner) = self.tasks.owner_of(message.sender, tick) else {
            return Err(err(errno::EACCES));
        };
        let result = self.socket_call(message, owner, method, body, now_ms);
        if self.sock_stats.owner_denied.replace(false) {
            self.note_denied(owner, method);
        }
        result
    }

    /// Count (and, a few times, log) a refusal: every one is audited.
    fn note_denied(&mut self, owner: u64, method: u32) {
        self.sock_stats.not_owner += 1;
        if self.sock_stats.denials_logged < DENY_LOG_LIMIT {
            self.sock_stats.denials_logged += 1;
            sys::write_str(&alloc::format!(
                "NETD:DENY owner={owner:#x} method={method}\n"
            ));
        }
    }

    fn socket_call(
        &mut self,
        message: &Message,
        owner: u64,
        method: u32,
        body: &[u8],
        now_ms: i64,
    ) -> Result<Option<Parcel>> {
        let done = |parcel: Result<Parcel>| parcel.map(Some);
        let empty = || Ok(Some(api::parcel(method, Vec::new())));
        match method {
            wire::METHOD_OPEN => {
                let args = wire::decode_open_args(body).map_err(MsgError::Parcel)?;
                let kind = match args.kind {
                    wire::SOCK_KIND_STREAM => Kind::Stream,
                    wire::SOCK_KIND_DATAGRAM => Kind::Datagram,
                    _ => return Err(err(errno::EINVAL)),
                };
                let sock = self
                    .net
                    .socket_open(owner, kind)
                    .map_err(|e| self.sock_error(e))?;
                done(reply(
                    method,
                    wire::encode_open_reply(&wire::OpenReply { sock }),
                ))
            }
            wire::METHOD_BIND => {
                let args = wire::decode_bind_args(body).map_err(MsgError::Parcel)?;
                let local = sock_addr(&args.addr)?;
                self.net
                    .socket_bind(args.sock, owner, local)
                    .map_err(|e| self.sock_error(e))?;
                empty()
            }
            wire::METHOD_CONNECT => {
                let args = wire::decode_connect_args(body).map_err(MsgError::Parcel)?;
                let wait = wait_ms(args.timeout_ms)?;
                let peer = sock_addr(&args.addr)?;
                // A call that timed out left the handshake running: asking
                // again waits for the same attempt instead of starting one.
                match self.net.socket_connect_status(args.sock, owner) {
                    Ok(false) => {}
                    Err(error) if error != SockError::NotConnected => {
                        return Err(self.sock_error(error))
                    }
                    _ => self
                        .net
                        .socket_connect(args.sock, owner, peer)
                        .map_err(|e| self.sock_error(e))?,
                }
                self.park(message, owner, args.sock, Op::Connect, wait, now_ms)
            }
            wire::METHOD_LISTEN => {
                let args = wire::decode_listen_args(body).map_err(MsgError::Parcel)?;
                self.net
                    .socket_listen(args.sock, owner, args.backlog)
                    .map_err(|e| self.sock_error(e))?;
                empty()
            }
            wire::METHOD_ACCEPT => {
                let args = wire::decode_accept_args(body).map_err(MsgError::Parcel)?;
                let wait = wait_ms(args.timeout_ms)?;
                self.park(message, owner, args.sock, Op::Accept, wait, now_ms)
            }
            wire::METHOD_SEND => {
                let args = wire::decode_send_args(body).map_err(MsgError::Parcel)?;
                let wait = wait_ms(args.timeout_ms)?;
                if args.data.is_empty() || args.data.len() > MAX_CHUNK {
                    return Err(err(errno::EINVAL));
                }
                self.park(message, owner, args.sock, Op::Send(args.data), wait, now_ms)
            }
            wire::METHOD_RECV => {
                let args = wire::decode_recv_args(body).map_err(MsgError::Parcel)?;
                let wait = wait_ms(args.timeout_ms)?;
                let max = args.max as usize;
                self.park(message, owner, args.sock, Op::Recv(max), wait, now_ms)
            }
            wire::METHOD_SENDTO => {
                let args = wire::decode_send_to_args(body).map_err(MsgError::Parcel)?;
                let to = sock_addr(&args.addr)?;
                let sent = self
                    .net
                    .socket_sendto(args.sock, owner, to, &args.data)
                    .map_err(|e| self.sock_error(e))?;
                let sent = sent as u32;
                done(reply(
                    method,
                    wire::encode_send_to_reply(&wire::SendToReply { sent }),
                ))
            }
            wire::METHOD_RECVFROM => {
                let args = wire::decode_recv_from_args(body).map_err(MsgError::Parcel)?;
                let wait = wait_ms(args.timeout_ms)?;
                let max = args.max as usize;
                self.park(message, owner, args.sock, Op::RecvFrom(max), wait, now_ms)
            }
            wire::METHOD_POLL => {
                let args = wire::decode_poll_args(body).map_err(MsgError::Parcel)?;
                let wait = wait_ms(args.timeout_ms)?;
                if args.interest == 0 || args.interest > 0x1F {
                    return Err(err(errno::EINVAL));
                }
                self.park(
                    message,
                    owner,
                    args.sock,
                    Op::Poll(args.interest),
                    wait,
                    now_ms,
                )
            }
            wire::METHOD_SHUTDOWN => {
                let args = wire::decode_shutdown_args(body).map_err(MsgError::Parcel)?;
                let (read, write) = match args.how {
                    wire::SHUTDOWN_READ => (true, false),
                    wire::SHUTDOWN_WRITE => (false, true),
                    wire::SHUTDOWN_BOTH => (true, true),
                    _ => return Err(err(errno::EINVAL)),
                };
                self.net
                    .socket_shutdown(args.sock, owner, read, write)
                    .map_err(|e| self.sock_error(e))?;
                empty()
            }
            wire::METHOD_LOCALADDR => {
                let args = wire::decode_local_addr_args(body).map_err(MsgError::Parcel)?;
                let addr = self
                    .net
                    .socket_local_addr(args.sock, owner)
                    .map_err(|e| self.sock_error(e))?;
                let addr = wire_addr(addr);
                done(reply(
                    method,
                    wire::encode_local_addr_reply(&wire::LocalAddrReply { addr }),
                ))
            }
            wire::METHOD_PEERADDR => {
                let args = wire::decode_peer_addr_args(body).map_err(MsgError::Parcel)?;
                let addr = self
                    .net
                    .socket_peer_addr(args.sock, owner)
                    .map_err(|e| self.sock_error(e))?;
                let addr = wire_addr(addr);
                done(reply(
                    method,
                    wire::encode_peer_addr_reply(&wire::PeerAddrReply { addr }),
                ))
            }
            wire::METHOD_CLOSE => {
                let args = wire::decode_close_args(body).map_err(MsgError::Parcel)?;
                self.net
                    .socket_close(args.sock, owner, now_ms)
                    .map_err(|e| self.sock_error(e))?;
                self.drop_calls_on(owner, args.sock);
                empty()
            }
            wire::METHOD_STATS => done(reply(
                method,
                wire::encode_stats_reply(&wire::StatsReply {
                    stats: self.socket_stats(),
                }),
            )),
            _ => Err(err(errno::EINVAL)),
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
