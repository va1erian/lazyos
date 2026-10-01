//! `AF_INET` for the Linux ABI (docs/networking-plan.md, stage N5): the socket
//! calls on an `Fd::Inet`, built on [`crate::ipc::inet`]. `socket.rs` hands a
//! call here when the descriptor (or the requested domain) is `AF_INET`.
//!
//! Streams are plain reads and writes on the socket's data path. A datagram
//! socket's path is a `SOCK_SEQPACKET` pair whose every message starts with
//! the peer's address ([`DGRAM_HEADER`] bytes), so `sendto` prepends it and
//! `recvfrom` strips it. Per-call `MSG_DONTWAIT` and `MSG_PEEK` are not
//! honoured (the descriptor's own `O_NONBLOCK` is).

use alloc::sync::Arc;

use crate::ipc::inet::{self, Addr, InetSock, Kind, State, DGRAM_HEADER, MAX_DGRAM};
use crate::task::{self, Fd, FdKind};
use crate::user_ptr;

use super::errno::{err, EBADF, EFAULT, EINVAL, EMFILE, EMSGSIZE, ENOPROTOOPT, ENOTSOCK};
use super::flags::{SOCK_CLOEXEC, SOCK_NONBLOCK, SOCK_STREAM};
use super::io::{read_stream, write_stream};

/// `AF_INET` and the sockaddr layout.
pub(super) const AF_INET: u64 = 2;
const SOCK_DGRAM: u64 = 2;
const IPPROTO_TCP: u64 = 6;
const IPPROTO_UDP: u64 = 17;
const SOCKADDR_IN: usize = 16;

/// Errno values Linux uses that `errno.rs` does not carry (positive).
const EPROTONOSUPPORT: u64 = 93;

const EDESTADDRREQ: u64 = 89;
const ENFILE: u64 = 23;
const ENOTCONN: u64 = 107;

/// `getsockopt` option names handled.
const SOL_SOCKET: u64 = 1;
const SO_TYPE: u64 = 3;
const SO_ERROR: u64 = 4;
const SO_SNDBUF: u64 = 7;
const SO_RCVBUF: u64 = 8;
const SO_ACCEPTCONN: u64 = 30;

fn code(error: i32) -> u64 {
    err(error as u64)
}

/// The inet socket behind `fd`, if it is one.
fn inet_of(fd: u64) -> Result<Arc<InetSock>, u64> {
    match task::fd_clone(fd as usize).as_ref() {
        Some(Fd::Inet { sock }) => Ok(Arc::clone(sock)),
        Some(Fd::Closed) | None => Err(err(EBADF)),
        Some(_) => Err(err(ENOTSOCK)),
    }
}

/// Whether `fd` is an `AF_INET` socket (the other socket calls defer to this).
pub(super) fn is_inet(fd: u64) -> bool {
    task::fd_kind(fd as usize) == FdKind::Inet
}

/// Parse a user `struct sockaddr_in`.
fn parse_addr(addr: u64, len: u64) -> Result<Addr, u64> {
    if (len as usize) < SOCKADDR_IN {
        return Err(err(EINVAL));
    }
    let raw = user_ptr::try_bytes(addr, SOCKADDR_IN).map_err(|_| err(EFAULT))?;
    if u16::from_le_bytes([raw[0], raw[1]]) as u64 != AF_INET {
        return Err(err(97)); // EAFNOSUPPORT
    }
    Ok(Addr {
        ip: [raw[4], raw[5], raw[6], raw[7]],
        port: u16::from_be_bytes([raw[2], raw[3]]),
    })
}

/// Write a `struct sockaddr_in` and its length to the user pointers (either
/// may be null).
fn write_addr(addr: u64, addrlen: u64, value: Addr) {
    if addr == 0 {
        return;
    }
    let mut raw = [0u8; SOCKADDR_IN];
    raw[0..2].copy_from_slice(&(AF_INET as u16).to_le_bytes());
    raw[2..4].copy_from_slice(&value.port.to_be_bytes());
    raw[4..8].copy_from_slice(&value.ip);
    // The caller's buffer may be smaller than the struct: copy what fits and
    // report the full size, as Linux does.
    let room = if addrlen == 0 {
        SOCKADDR_IN
    } else {
        user_ptr::try_read::<u32>(addrlen).map_or(0, |n| n as usize)
    };
    let n = room.min(SOCKADDR_IN);
    let _ = user_ptr::try_copy_to(addr, &raw[..n]);
    if addrlen != 0 {
        let _ = user_ptr::try_write::<u32>(addrlen, SOCKADDR_IN as u32);
    }
}

/// `socket(AF_INET, type, protocol)`.
pub(super) fn sys_socket(kind: u64, protocol: u64) -> u64 {
    let (sock_kind, wanted) = match kind & 0xf {
        SOCK_STREAM => (Kind::Stream, IPPROTO_TCP),
        SOCK_DGRAM => (Kind::Dgram, IPPROTO_UDP),
        _ => return err(EINVAL),
    };
    if protocol != 0 && protocol != wanted {
        return err(EPROTONOSUPPORT);
    }
    let Some(sock) = inet::create(sock_kind) else {
        return err(ENFILE);
    };
    if kind & SOCK_NONBLOCK != 0 {
        sock.set_nonblock(true);
    }
    match task::fd_open(Fd::Inet { sock }) {
        Some(fd) => {
            if kind & SOCK_CLOEXEC != 0 {
                task::fd_set_cloexec(fd, true);
            }
            fd as u64
        }
        None => err(EMFILE),
    }
}

pub(super) fn sys_bind(fd: u64, addr: u64, len: u64) -> u64 {
    let sock = match inet_of(fd) {
        Ok(sock) => sock,
        Err(e) => return e,
    };
    let addr = match parse_addr(addr, len) {
        Ok(addr) => addr,
        Err(e) => return e,
    };
    match sock.begin_bind(addr).and_then(|t| sock.finish(t)) {
        Ok(()) => 0,
        Err(e) => code(e),
    }
}

pub(super) fn sys_connect(fd: u64, addr: u64, len: u64) -> u64 {
    let sock = match inet_of(fd) {
        Ok(sock) => sock,
        Err(e) => return e,
    };
    let addr = match parse_addr(addr, len) {
        Ok(addr) => addr,
        Err(e) => return e,
    };
    match sock.begin_connect(addr) {
        Ok(None) => 0,
        Ok(Some(ticket)) => match sock.finish(ticket) {
            Ok(()) => 0,
            Err(e) => code(e),
        },
        Err(e) => code(e),
    }
}

pub(super) fn sys_listen(fd: u64, backlog: u64) -> u64 {
    let sock = match inet_of(fd) {
        Ok(sock) => sock,
        Err(e) => return e,
    };
    match sock
        .begin_listen(backlog.min(u32::MAX as u64) as u32)
        .and_then(|t| sock.finish(t))
    {
        Ok(()) => 0,
        Err(e) => code(e),
    }
}

pub(super) fn sys_accept(fd: u64, addr: u64, addrlen: u64, flags: u64) -> u64 {
    let sock = match inet_of(fd) {
        Ok(sock) => sock,
        Err(e) => return e,
    };
    let conn = match sock.accept() {
        Ok(conn) => conn,
        Err(e) => return code(e),
    };
    let peer = conn.peer().unwrap_or(Addr::ANY);
    if flags & SOCK_NONBLOCK != 0 {
        conn.set_nonblock(true);
    }
    let Some(new_fd) = task::fd_open(Fd::Inet { sock: conn }) else {
        return err(EMFILE);
    };
    if flags & SOCK_CLOEXEC != 0 {
        task::fd_set_cloexec(new_fd, true);
    }
    write_addr(addr, addrlen, peer);
    new_fd as u64
}

pub(super) fn sys_shutdown(fd: u64, how: u64) -> u64 {
    match inet_of(fd).map(|sock| sock.shutdown(how)) {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => code(e),
        Err(e) => e,
    }
}

/// `getsockname` (51) and `getpeername` (52).
pub(super) fn sys_name(fd: u64, addr: u64, addrlen: u64, peer: bool) -> u64 {
    let sock = match inet_of(fd) {
        Ok(sock) => sock,
        Err(e) => return e,
    };
    let value = if peer {
        match sock.peer() {
            Some(peer) => peer,
            None => return err(ENOTCONN),
        }
    } else {
        sock.local()
    };
    write_addr(addr, addrlen, value);
    0
}

/// Bind a datagram socket that has not been (an implicit bind to a free port).
fn ensure_bound(sock: &InetSock) -> Result<(), u64> {
    if sock.state() != State::Fresh {
        return Ok(());
    }
    sock.begin_bind(Addr::ANY)
        .and_then(|t| sock.finish(t))
        .map_err(code)
}

/// `sendto(fd, buf, len, flags, addr, addrlen)`.
pub(super) fn sys_sendto(fd: u64, buf: u64, len: u64, addr: u64, addrlen: u64) -> u64 {
    let sock = match inet_of(fd) {
        Ok(sock) => sock,
        Err(e) => return e,
    };
    if sock.kind() == Kind::Stream {
        if sock.pair().is_none() {
            return err(ENOTCONN);
        }
        return write_stream(fd, buf, len);
    }
    let to = if addr != 0 {
        match parse_addr(addr, addrlen) {
            Ok(to) => to,
            Err(e) => return e,
        }
    } else {
        match sock.peer() {
            Some(peer) => peer,
            None => return err(EDESTADDRREQ),
        }
    };
    send_datagram(&sock, fd, buf, len, to)
}

fn send_datagram(sock: &InetSock, fd: u64, buf: u64, len: u64, to: Addr) -> u64 {
    if len as usize > MAX_DGRAM {
        return err(EMSGSIZE);
    }
    if to.port == 0 {
        return err(EINVAL);
    }
    if let Err(e) = ensure_bound(sock) {
        return e;
    }
    let payload = match user_ptr::try_bytes(buf, len as usize) {
        Ok(bytes) => bytes,
        Err(_) => return err(EFAULT),
    };
    let mut message = alloc::vec::Vec::with_capacity(DGRAM_HEADER + payload.len());
    message.extend_from_slice(&to.ip);
    message.extend_from_slice(&to.port.to_be_bytes());
    message.extend_from_slice(payload);
    match task::fd_stream_write(fd as usize, &message) {
        Ok(_) => len,
        Err(e) => super::io::pipe_error(e),
    }
}

/// `recvfrom(fd, buf, len, flags, addr, addrlen)`.
pub(super) fn sys_recvfrom(fd: u64, buf: u64, len: u64, addr: u64, addrlen: u64) -> u64 {
    let sock = match inet_of(fd) {
        Ok(sock) => sock,
        Err(e) => return e,
    };
    if sock.kind() == Kind::Stream {
        if sock.pair().is_none() {
            return err(ENOTCONN);
        }
        let n = read_stream(fd, buf, len);
        if addr != 0 && (n as i64) >= 0 {
            write_addr(addr, addrlen, sock.peer().unwrap_or(Addr::ANY));
        }
        return n;
    }
    recv_datagram(&sock, fd, buf, len, addr, addrlen)
}

fn recv_datagram(sock: &InetSock, fd: u64, buf: u64, len: u64, addr: u64, addrlen: u64) -> u64 {
    if let Err(e) = ensure_bound(sock) {
        return e;
    }
    let want = (len as usize).min(MAX_DGRAM);
    let mut message = alloc::vec![0u8; DGRAM_HEADER + want];
    let n = match task::fd_stream_read(fd as usize, &mut message) {
        Ok(n) => n,
        Err(e) => return super::io::pipe_error(e),
    };
    if n < DGRAM_HEADER {
        return err(EINVAL);
    }
    let from = Addr {
        ip: [message[0], message[1], message[2], message[3]],
        port: u16::from_be_bytes([message[4], message[5]]),
    };
    let payload = &message[DGRAM_HEADER..n];
    if user_ptr::try_copy_to(buf, payload).is_err() {
        return err(EFAULT);
    }
    write_addr(addr, addrlen, from);
    payload.len() as u64
}

/// `read(2)` on an inet socket.
pub(super) fn sys_read(fd: u64, buf: u64, len: u64) -> u64 {
    sys_recvfrom(fd, buf, len, 0, 0)
}

/// `write(2)` on an inet socket.
pub(super) fn sys_write(fd: u64, buf: u64, len: u64) -> u64 {
    sys_sendto(fd, buf, len, 0, 0)
}

/// `setsockopt(fd, level, name, value, len)`: the options programs set on a
/// socket as a matter of course are accepted and have no effect (buffer
/// sizes, keepalive, address reuse, `TCP_NODELAY`, timeouts); the value
/// pointer is still validated.
pub(super) fn sys_setsockopt(fd: u64, _level: u64, _name: u64, value: u64, len: u64) -> u64 {
    if let Err(e) = inet_of(fd) {
        return e;
    }
    if len > 0 && user_ptr::try_bytes(value, (len as usize).min(256)).is_err() {
        return err(EFAULT);
    }
    0
}

/// `getsockopt(fd, level, name, value, lenptr)`.
pub(super) fn sys_getsockopt(fd: u64, level: u64, name: u64, value: u64, lenptr: u64) -> u64 {
    let sock = match inet_of(fd) {
        Ok(sock) => sock,
        Err(e) => return e,
    };
    if level != SOL_SOCKET {
        return err(ENOPROTOOPT);
    }
    let answer: i32 = match name {
        SO_ERROR => sock.take_error(),
        SO_TYPE => match sock.kind() {
            Kind::Stream => SOCK_STREAM as i32,
            Kind::Dgram => SOCK_DGRAM as i32,
        },
        SO_RCVBUF | SO_SNDBUF => crate::ipc::pipe::SMALL_CAPACITY as i32,
        SO_ACCEPTCONN => i32::from(sock.state() == State::Listening),
        _ => 0,
    };
    let Ok(room) = user_ptr::try_read::<u32>(lenptr) else {
        return err(EFAULT);
    };
    let bytes = answer.to_le_bytes();
    let n = (room as usize).min(4);
    if user_ptr::try_copy_to(value, &bytes[..n]).is_err()
        || user_ptr::try_write::<u32>(lenptr, 4).is_err()
    {
        return err(EFAULT);
    }
    0
}
