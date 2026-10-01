//! Unix-domain sockets (and the `AF_INET` dispatch to `inet.rs`): `socket`, `bind`, `listen`, `connect`, `accept`/
//! `accept4`, `shutdown`, `getsockname`/`getpeername`, and `sendto`/`recvfrom`
//! (musl's `send`/`recv`). `AF_UNIX` is the only family; a socket is unbound
//! until `bind` or `connect` turns it into a listener or a connected pair
//! ([`crate::ipc::pipe::SocketPair`]), at which point `read`/`write` on it are
//! just [`super::io`]'s stream handlers.

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::ipc::pipe::{Side, SocketPair};
use crate::ipc::unix;
use crate::task::{self, Fd, FdKind, SocketKind, WakeReason};
use crate::user_ptr;

use super::errno::{
    err, EADDRINUSE, EAFNOSUPPORT, EAGAIN, EBADF, ECONNREFUSED, EINTR, EINVAL, EMFILE, ENOENT,
    ENOPROTOOPT, ENOTCONN, ENOTSOCK,
};
use super::flags::{AF_UNIX, SOCK_CLOEXEC, SOCK_NONBLOCK, SOCK_SEQPACKET, SOCK_STREAM};
use super::io::{read_stream, write_stream};

/// `shutdown(2)` directions.
const SHUT_RD: u64 = 0;
const SHUT_WR: u64 = 1;
const SHUT_RDWR: u64 = 2;

/// `socket(domain, type, protocol)`: only `AF_UNIX`; the descriptor stays
/// unbound until `bind` or `connect`.
pub(super) fn sys_socket(domain: u64, kind: u64, protocol: u64) -> u64 {
    if domain == super::inet::AF_INET {
        return super::inet::sys_socket(kind, protocol);
    }
    if domain != AF_UNIX {
        return err(EAFNOSUPPORT);
    }
    let base = kind & 0xf;
    let socket_kind = match base {
        SOCK_STREAM => SocketKind::Stream,
        SOCK_SEQPACKET => SocketKind::Seqpacket,
        _ => return err(EINVAL),
    };
    if protocol != 0 {
        return err(EINVAL);
    }
    let nonblock = kind & SOCK_NONBLOCK != 0;
    match task::fd_open(Fd::Unbound {
        kind: socket_kind,
        nonblock,
    }) {
        Some(fd) => {
            if kind & SOCK_CLOEXEC != 0 {
                task::fd_set_cloexec(fd, true);
            }
            fd as u64
        }
        None => err(EMFILE),
    }
}

/// Parse a user `struct sockaddr_un` into a bound-name key. A filesystem path
/// loses its NUL terminator; an abstract name keeps its leading NUL byte so it
/// can never collide with a path.
fn parse_unix_name(addr: u64, len: u64) -> Result<Vec<u8>, u64> {
    if len < 3 {
        return Err(EINVAL);
    }
    // Safety: user `struct sockaddr_un` (the syscall ABI's contract).
    let family = unsafe { user_ptr::read::<u16>(addr) };
    if family as u64 != AF_UNIX {
        return Err(EAFNOSUPPORT);
    }
    let available = ((len - 2) as usize).min(108);
    // Safety: same struct, `sun_path` follows `sun_family`.
    let path = unsafe { user_ptr::bytes(addr + 2, available) };
    if path.first() == Some(&0) {
        Ok(path.to_vec())
    } else {
        let end = path
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(path.len());
        if end == 0 {
            return Err(EINVAL);
        }
        Ok(path[..end].to_vec())
    }
}

/// Write a user `struct sockaddr_un` (and its length) for `name`.
fn write_unix_name(addr: u64, addrlen: u64, name: &[u8]) {
    let n = name.len().min(108);
    let mut buf = [0u8; 110];
    buf[..2].copy_from_slice(&(AF_UNIX as u16).to_le_bytes());
    buf[2..2 + n].copy_from_slice(&name[..n]);
    // Safety: user `struct sockaddr_un` and `socklen_t *` (the syscall ABI's
    // contract).
    unsafe {
        user_ptr::copy_to(addr, &buf[..2 + n]);
        if addrlen != 0 {
            user_ptr::write::<u32>(addrlen, (2 + n) as u32);
        }
    }
}

/// `bind(fd, addr, len)`: attach the unbound socket to a name and turn it into
/// a listener.
pub(super) fn sys_bind(fd: u64, addr: u64, len: u64) -> u64 {
    if super::inet::is_inet(fd) {
        return super::inet::sys_bind(fd, addr, len);
    }
    let Some(target) = task::fd_clone(fd as usize) else {
        return err(EBADF);
    };
    let Fd::Unbound { nonblock, .. } = target else {
        return err(EINVAL);
    };
    let name = match parse_unix_name(addr, len) {
        Ok(name) => name,
        Err(error) => return err(error),
    };
    let listener = match unix::bind(name) {
        Ok(listener) => listener,
        Err(()) => return err(EADDRINUSE),
    };
    if nonblock {
        listener.set_nonblock(true);
    }
    match task::fd_replace(fd as usize, Fd::UnixListener { listener }) {
        Ok(old) => {
            drop(old);
            0
        }
        Err(()) => err(EBADF),
    }
}

/// `listen(fd, backlog)`: mark a bound socket connectable (backlog ignored).
pub(super) fn sys_listen(fd: u64, backlog: u64) -> u64 {
    if super::inet::is_inet(fd) {
        return super::inet::sys_listen(fd, backlog);
    }
    match task::fd_clone(fd as usize).as_ref() {
        Some(Fd::UnixListener { listener }) => {
            listener.listen();
            0
        }
        Some(_) => err(EINVAL),
        None => err(EBADF),
    }
}

/// `connect(fd, addr, len)`: connect an unbound socket to a listening name.
/// The connection is a fresh [`SocketPair`]; the server half waits in the
/// listener for `accept`.
pub(super) fn sys_connect(fd: u64, addr: u64, len: u64) -> u64 {
    if super::inet::is_inet(fd) {
        return super::inet::sys_connect(fd, addr, len);
    }
    let Some(target) = task::fd_clone(fd as usize) else {
        return err(EBADF);
    };
    let Fd::Unbound { kind, nonblock } = target else {
        return err(EINVAL);
    };
    let name = match parse_unix_name(addr, len) {
        Ok(name) => name,
        Err(error) => return err(error),
    };
    let Some(listener) = unix::lookup(&name) else {
        return err(ENOENT);
    };
    if !listener.is_listening() {
        return err(ECONNREFUSED);
    }
    let pair = match kind {
        SocketKind::Stream => SocketPair::new(),
        SocketKind::Seqpacket => SocketPair::new_seqpacket(),
    };
    let Some(pair) = pair else {
        return err(EMFILE);
    };
    if nonblock {
        pair.set_nonblock(Side::B, true);
    }
    listener.connect(Arc::clone(&pair));
    match task::fd_replace(fd as usize, Fd::socket_side(pair, Side::B)) {
        Ok(old) => {
            drop(old);
            0
        }
        Err(()) => err(EBADF),
    }
}

/// `accept` (43) and `accept4` (288): take the next pending connection from a
/// listener, parking while none is pending unless non-blocking.
pub(super) fn sys_accept(fd: u64, addr: u64, addrlen: u64, flags: u64) -> u64 {
    if super::inet::is_inet(fd) {
        return super::inet::sys_accept(fd, addr, addrlen, flags);
    }
    let Some(target) = task::fd_clone(fd as usize) else {
        return err(EBADF);
    };
    let Fd::UnixListener { listener } = &target else {
        return err(EINVAL);
    };
    let pair = loop {
        if let Some(pair) = listener.take_pending() {
            break pair;
        }
        if listener.nonblock() {
            return err(EAGAIN);
        }
        match listener.wait_connection() {
            WakeReason::Woken | WakeReason::TimedOut => {}
            WakeReason::Interrupted => return err(EINTR),
        }
    };
    // The listener took this side's reference at `connect`; adopt it (a
    // failed `fd_open` drops the `Fd`, which releases it).
    let Some(new_fd) = task::fd_open(Fd::socket_side_adopt(pair, Side::A)) else {
        return err(EMFILE);
    };
    if flags & SOCK_CLOEXEC != 0 {
        task::fd_set_cloexec(new_fd, true);
    }
    if flags & SOCK_NONBLOCK != 0 {
        if let Some(Fd::Socket { pair, side }) = task::fd_clone(new_fd).as_ref() {
            pair.set_nonblock(*side, true);
        }
    }
    if addr != 0 {
        write_unix_name(addr, addrlen, &[]);
    }
    new_fd as u64
}

/// `shutdown(fd, how)`: close one direction of a connected socket pair.
pub(super) fn sys_shutdown(fd: u64, how: u64) -> u64 {
    if super::inet::is_inet(fd) {
        return super::inet::sys_shutdown(fd, how);
    }
    if how != SHUT_RD && how != SHUT_WR && how != SHUT_RDWR {
        return err(EINVAL);
    }
    match task::fd_clone(fd as usize).as_ref() {
        Some(Fd::Socket { pair, side }) => {
            pair.shutdown(*side, how);
            0
        }
        Some(Fd::Unbound { .. }) => err(ENOTCONN),
        Some(_) => err(ENOTSOCK),
        None => err(EBADF),
    }
}

/// `getsockname`/`getpeername`: a bound listener reports its name, a connected
/// pair reports an empty path (peer names are not tracked).
pub(super) fn sys_get_sockname(fd: u64, addr: u64, addrlen: u64, peer: bool) -> u64 {
    if super::inet::is_inet(fd) {
        return super::inet::sys_name(fd, addr, addrlen, peer);
    }
    let Some(target) = task::fd_clone(fd as usize) else {
        return err(EBADF);
    };
    match &target {
        Fd::UnixListener { listener } => {
            if addr != 0 {
                write_unix_name(addr, addrlen, &listener.name);
            }
            0
        }
        Fd::Socket { .. } | Fd::Unbound { .. } => {
            if addr != 0 {
                write_unix_name(addr, addrlen, &[]);
            }
            0
        }
        _ => err(ENOTSOCK),
    }
}

/// `sendto(fd, buf, len, flags, addr, addrlen)`: musl's `send`. The only
/// sockets are connected `AF_UNIX` pairs, so the destination is ignored (std
/// passes a null address) and this is a stream write; a non-socket fd is
/// `-ENOTSOCK`, as Linux reports.
pub(super) fn sys_sendto(fd: u64, buf: u64, len: u64, addr: u64, addrlen: u64) -> u64 {
    if super::inet::is_inet(fd) {
        return super::inet::sys_sendto(fd, buf, len, addr, addrlen);
    }
    match task::fd_kind(fd as usize) {
        FdKind::Socket => write_stream(fd, buf, len),
        _ => err(ENOTSOCK),
    }
}

/// `recvfrom(fd, buf, len, flags, addr, addrlen)`: musl's `recv`. Source
/// addresses do not exist for connected pairs (std passes null), so this is a
/// stream read; a non-socket fd is `-ENOTSOCK`.
pub(super) fn sys_recvfrom(fd: u64, buf: u64, len: u64, addr: u64, addrlen: u64) -> u64 {
    if super::inet::is_inet(fd) {
        return super::inet::sys_recvfrom(fd, buf, len, addr, addrlen);
    }
    match task::fd_kind(fd as usize) {
        FdKind::Socket => read_stream(fd, buf, len),
        _ => err(ENOTSOCK),
    }
}

/// `setsockopt` (54): only `AF_INET` sockets take options so far.
pub(super) fn sys_setsockopt(fd: u64, level: u64, name: u64, value: u64, len: u64) -> u64 {
    match task::fd_kind(fd as usize) {
        FdKind::Inet => super::inet::sys_setsockopt(fd, level, name, value, len),
        FdKind::Closed => err(EBADF),
        kind => err(no_option(kind)),
    }
}

/// `getsockopt` (55): see [`sys_setsockopt`].
pub(super) fn sys_getsockopt(fd: u64, level: u64, name: u64, value: u64, lenptr: u64) -> u64 {
    match task::fd_kind(fd as usize) {
        FdKind::Inet => super::inet::sys_getsockopt(fd, level, name, value, lenptr),
        FdKind::Closed => err(EBADF),
        kind => err(no_option(kind)),
    }
}

/// What a descriptor that is not an `AF_INET` socket answers to an option call:
/// the other sockets have no such option, everything else is not a socket.
fn no_option(kind: FdKind) -> u64 {
    match kind {
        FdKind::Socket | FdKind::Listener | FdKind::Unbound => ENOPROTOOPT,
        _ => ENOTSOCK,
    }
}
