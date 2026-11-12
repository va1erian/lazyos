//! `os.lazy.net.socket.v1` client: sockets served by `netd`
//! (`idl/net.midl`, docs/networking-plan.md N3).
//!
//! The stubs and the generated client ([`wire::Client`]) come from `midlc`;
//! this module adds the domain meaning and no framing: [`Addr`] (an IPv4
//! address and port instead of the wire's byte vector), the timeout clamp, the
//! chunk cap, the bound of a parked call, and the errno values the interface
//! reports. The std-like [`TcpStream`](super::netstd::TcpStream) and friends
//! in `netstd.rs` are built on [`Client`].
//!
//! **Waiting.** `Connect`, `Accept`, `Send`, `Recv`, `RecvFrom` and `Poll`
//! park inside `netd`, which answers when the socket is ready or after
//! `timeout_ms` with `ETIMEDOUT`. The caller's own bound is set a little
//! beyond that, so `netd`'s answer, not the caller's deadline, ends a wait.
//! The service endpoint is the stack service's (`os.lazy.net.stack`): `netd`
//! tells the interfaces apart by id.

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::client::{CallError, Deadline, Errno};
use libmessenger::{Header, Parcel, VERSION};

use super::client::Native;

type Result<T> = core::result::Result<T, CallError>;

/// The generated `os.lazy.net.socket.v1` stubs.
pub use messenger_generated::os_lazy_net_socket_v1 as wire;

/// Registered service name: the socket interface rides on `netd`'s endpoint.
pub const NAME: &str = super::netstack::NAME;

/// The interface id every socket parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// Errno values (Linux numbering) `netd` reports for the socket interface.
pub mod errno {
    pub const ENOENT: i64 = 2;
    pub const EIO: i64 = 5;
    pub const EBADF: i64 = 9;
    pub const EACCES: i64 = 13;
    pub const EINVAL: i64 = 22;
    pub const ENFILE: i64 = 23;
    pub const EMFILE: i64 = 24;
    pub const EMSGSIZE: i64 = 90;
    pub const EADDRINUSE: i64 = 98;
    pub const ENETUNREACH: i64 = 101;
    pub const ETIMEDOUT: i64 = 110;
    pub const ECONNRESET: i64 = 104;
    pub const ENOTCONN: i64 = 107;
    pub const ECONNREFUSED: i64 = 111;
}

/// Whether `error` carries the (positive) errno `code`: a transport failure
/// or a refusal by the service, which report it the same way.
pub fn is_errno(error: &CallError, code: i64) -> bool {
    error.code() == -code
}

/// A short sentence for a failure of a socket or stack call, for the tools
/// that print it (`nc`, `ftp`, `smb`).
pub fn describe(error: &CallError) -> String {
    let text = match -error.code() {
        errno::ECONNREFUSED => "connection refused",
        errno::ENETUNREACH => "network is unreachable",
        errno::ECONNRESET => "connection reset",
        errno::ETIMEDOUT => "timed out",
        _ => return String::from(error.message()),
    };
    String::from(text)
}

/// Largest `Send` and `Recv` chunk.
pub const MAX_CHUNK: usize = 16 * 1024;
/// The longest a call may park (`timeout_ms` 0 means this).
pub const MAX_TIMEOUT_MS: u32 = 60_000;
/// Time a caller waits beyond a call's own timeout.
const DEADLINE_SLACK_MS: u64 = 1_000;

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

    pub fn new(ip: [u8; 4], port: u16) -> Addr {
        Addr { ip, port }
    }

    fn to_wire(self) -> wire::SockAddr {
        wire::SockAddr {
            addr: self.ip.to_vec(),
            port: u32::from(self.port),
        }
    }

    fn from_wire(addr: &wire::SockAddr) -> Result<Addr> {
        let ip: [u8; 4] = addr
            .addr
            .as_slice()
            .try_into()
            .map_err(|_| CallError::Transport(Errno::BADMSG))?;
        let port = u16::try_from(addr.port).map_err(|_| CallError::Transport(Errno::BADMSG))?;
        Ok(Addr { ip, port })
    }
}

/// A parcel of `method` carrying an encoded `body` (the server's reply).
pub fn parcel(method: u32, body: Vec<u8>) -> Parcel {
    Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: INTERFACE,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        ..Parcel::default()
    }
}

/// The wire `timeout_ms` for a wait of at most `ms` (0: the longest).
fn clamp_timeout(ms: u32) -> u32 {
    match ms {
        0 => MAX_TIMEOUT_MS,
        ms => ms.clamp(10, MAX_TIMEOUT_MS),
    }
}

/// The bound of a call that may park for `timeout_ms` inside `netd`.
fn parked(timeout_ms: u32) -> Deadline {
    Deadline::millis(u64::from(timeout_ms) + DEADLINE_SLACK_MS)
}

/// A blocking client of the socket service: the generated client with
/// [`Addr`]s and the waits this interface documents.
pub struct Client {
    wire: wire::Client<Native>,
}

impl Client {
    /// Resolve [`NAME`] with one attempt: a caller connects to the stack
    /// first ([`super::netstack::Client`]), which is the one that waits.
    pub fn connect() -> Result<Client> {
        Ok(Client {
            wire: wire::Client::try_connect(Native)?,
        })
    }

    /// The generated client underneath, for a probe that frames a request by
    /// hand on its connection.
    pub fn wire(&self) -> &wire::Client<Native> {
        &self.wire
    }

    /// `Open`: a new socket of `kind` (`wire::SOCK_KIND_*`).
    pub fn open(&self, kind: u32) -> Result<u32> {
        self.wire.open(kind)
    }

    pub fn bind(&self, sock: u32, addr: Addr) -> Result<()> {
        self.wire.bind(sock, &addr.to_wire())
    }

    pub fn connect_to(&self, sock: u32, addr: Addr, timeout_ms: u32) -> Result<()> {
        let timeout_ms = clamp_timeout(timeout_ms);
        self.wire
            .connect_call_within(sock, &addr.to_wire(), timeout_ms, parked(timeout_ms))
    }

    pub fn listen(&self, sock: u32, backlog: u32) -> Result<()> {
        self.wire.listen(sock, backlog)
    }

    /// `Accept`: the new socket and the peer's address.
    pub fn accept(&self, sock: u32, timeout_ms: u32) -> Result<(u32, Addr)> {
        let timeout_ms = clamp_timeout(timeout_ms);
        let reply = self
            .wire
            .accept_within(sock, timeout_ms, parked(timeout_ms))?;
        Ok((reply.conn, Addr::from_wire(&reply.peer)?))
    }

    /// `Send`: how many bytes were queued (at most [`MAX_CHUNK`] are offered).
    pub fn send(&self, sock: u32, data: &[u8], timeout_ms: u32) -> Result<usize> {
        let timeout_ms = clamp_timeout(timeout_ms);
        let data = &data[..data.len().min(MAX_CHUNK)];
        let sent = self
            .wire
            .send_within(sock, data, timeout_ms, parked(timeout_ms))?;
        Ok(sent as usize)
    }

    /// `Recv`: up to `max` bytes; an empty vector is the end of the stream.
    pub fn recv(&self, sock: u32, max: usize, timeout_ms: u32) -> Result<Vec<u8>> {
        let timeout_ms = clamp_timeout(timeout_ms);
        self.wire.recv_within(
            sock,
            max.min(MAX_CHUNK) as u32,
            timeout_ms,
            parked(timeout_ms),
        )
    }

    pub fn send_to(&self, sock: u32, addr: Addr, data: &[u8]) -> Result<usize> {
        let sent = self.wire.send_to(sock, &addr.to_wire(), data)?;
        Ok(sent as usize)
    }

    pub fn recv_from(&self, sock: u32, max: usize, timeout_ms: u32) -> Result<(Vec<u8>, Addr)> {
        let timeout_ms = clamp_timeout(timeout_ms);
        let reply = self.wire.recv_from_within(
            sock,
            max.min(MAX_CHUNK) as u32,
            timeout_ms,
            parked(timeout_ms),
        )?;
        Ok((reply.data, Addr::from_wire(&reply.from)?))
    }

    /// `Poll`: the readiness bits (`1 << wire::READY_*`) among `interest`,
    /// plus `Closed` and `Error` whenever they hold.
    pub fn poll(&self, sock: u32, interest: u32, timeout_ms: u32) -> Result<u32> {
        let timeout_ms = clamp_timeout(timeout_ms);
        self.wire
            .poll_within(sock, interest, timeout_ms, parked(timeout_ms))
    }

    pub fn shutdown(&self, sock: u32, how: u32) -> Result<()> {
        self.wire.shutdown(sock, how)
    }

    pub fn local_addr(&self, sock: u32) -> Result<Addr> {
        Addr::from_wire(&self.wire.local_addr(sock)?)
    }

    pub fn peer_addr(&self, sock: u32) -> Result<Addr> {
        Addr::from_wire(&self.wire.peer_addr(sock)?)
    }

    pub fn close(&self, sock: u32) -> Result<()> {
        self.wire.close(sock)
    }

    pub fn stats(&self) -> Result<wire::SocketStats> {
        self.wire.stats()
    }
}
