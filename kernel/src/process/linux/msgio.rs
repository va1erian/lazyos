//! The socket transfer calls with flags: `sendto`/`recvfrom` and
//! `sendmsg`/`recvmsg`.
//!
//! Honoured flags: `MSG_DONTWAIT` (this call never blocks), `MSG_PEEK` (the
//! bytes stay queued), `MSG_WAITALL` (a stream read waits for the whole
//! buffer, short only at end-of-file, a signal or an error), and
//! `MSG_NOSIGNAL` (no `SIGPIPE` is ever raised here, so it changes nothing).
//! `MSG_MORE`, `MSG_EOR` and `MSG_CMSG_CLOEXEC` have no effect on these
//! sockets and are accepted; `MSG_OOB` and any unknown flag are refused with
//! `EOPNOTSUPP`/`EINVAL` rather than ignored. Ancillary data (`SCM_RIGHTS`
//! descriptor passing, credentials) is not supported: a `sendmsg` with control
//! data is `EOPNOTSUPP`, and `recvmsg` always reports none.

use alloc::vec::Vec;

use crate::task::{self, RecvOpts};
use crate::user_ptr;

use super::scatter::{Received, Scatter};

use super::errno::{err, EFAULT, EINVAL, EMSGSIZE, EOPNOTSUPP};

const MSG_OOB: u64 = 0x1;
const MSG_PEEK: u64 = 0x2;
const MSG_DONTWAIT: u64 = 0x40;
const MSG_EOR: u64 = 0x80;
const MSG_WAITALL: u64 = 0x100;
const MSG_NOSIGNAL: u64 = 0x4000;
const MSG_MORE: u64 = 0x8000;
const MSG_CMSG_CLOEXEC: u64 = 0x4000_0000;

const SEND_FLAGS: u64 = MSG_DONTWAIT | MSG_EOR | MSG_NOSIGNAL | MSG_MORE;
const RECV_FLAGS: u64 = MSG_PEEK | MSG_DONTWAIT | MSG_WAITALL | MSG_CMSG_CLOEXEC;

/// Most `iovec` entries one call takes (`UIO_MAXIOV`).
const IOV_MAX: u64 = 1024;

/// Decode the send flags: `Ok(dont_wait)` or the errno.
fn send_flags(flags: u64) -> Result<bool, u64> {
    let flags = flags & 0xffff_ffff;
    if flags & MSG_OOB != 0 {
        return Err(err(EOPNOTSUPP));
    }
    if flags & !SEND_FLAGS != 0 {
        return Err(err(EINVAL));
    }
    Ok(flags & MSG_DONTWAIT != 0)
}

/// Decode the receive flags.
fn recv_flags(flags: u64) -> Result<(RecvOpts, bool), u64> {
    let flags = flags & 0xffff_ffff;
    if flags & MSG_OOB != 0 {
        return Err(err(EOPNOTSUPP));
    }
    if flags & !RECV_FLAGS != 0 {
        return Err(err(EINVAL));
    }
    let opts = RecvOpts {
        dont_wait: flags & MSG_DONTWAIT != 0,
        peek: flags & MSG_PEEK != 0,
    };
    Ok((opts, flags & MSG_WAITALL != 0 && flags & MSG_PEEK == 0))
}

/// `sendto(fd, buf, len, flags, addr, addrlen)`.
pub(super) fn sys_sendto(fd: u64, buf: u64, len: u64, flags: u64, addr: u64, alen: u64) -> u64 {
    match send_flags(flags) {
        Ok(dont_wait) => super::socket::sys_sendto(fd, buf, len, (addr, alen), dont_wait),
        Err(code) => code,
    }
}

/// `recvfrom(fd, buf, len, flags, addr, addrlen)`.
pub(super) fn sys_recvfrom(fd: u64, buf: u64, len: u64, flags: u64, addr: u64, alen: u64) -> u64 {
    let (opts, wait_all) = match recv_flags(flags) {
        Ok(decoded) => decoded,
        Err(code) => return code,
    };
    receive(fd, &Scatter::one(buf, len), (addr, alen), opts, wait_all).result
}

/// One receive into `dest`. A message socket always reads exactly one
/// message. `MSG_WAITALL` on a stream keeps reading until `dest` is full; a
/// partial count is returned when a later read ends (EOF, signal, error), as
/// Linux does.
fn receive(fd: u64, dest: &Scatter, from: (u64, u64), opts: RecvOpts, wait_all: bool) -> Received {
    if !wait_all || is_message_socket(fd) {
        return super::socket::recv_into(fd, dest, from, opts);
    }
    let len = dest.len();
    let mut got = 0u64;
    while got < len {
        let n = super::socket::recv_into(fd, &dest.after(got), from, opts).result;
        if (n as i64) <= 0 {
            return Received::of(if got > 0 { got } else { n });
        }
        got += n;
    }
    Received::of(got)
}

/// The fields of a `struct msghdr` this layer uses.
struct MsgHdr {
    name: u64,
    namelen: u64,
    iov: u64,
    iovlen: u64,
    control: u64,
    controllen: u64,
}

fn read_msghdr(ptr: u64) -> Result<MsgHdr, u64> {
    let word = |at: u64| user_ptr::try_read::<u64>(ptr + at).map_err(|_| err(EFAULT));
    Ok(MsgHdr {
        name: word(0)?,
        namelen: u64::from(user_ptr::try_read::<u32>(ptr + 8).map_err(|_| err(EFAULT))?),
        iov: word(16)?,
        iovlen: word(24)?,
        control: word(32)?,
        controllen: word(40)?,
    })
}

/// The `(base, len)` pairs of an `iovec` array.
fn read_iov(iov: u64, count: u64) -> Result<Vec<(u64, u64)>, u64> {
    if count > IOV_MAX {
        return Err(err(EMSGSIZE));
    }
    let mut out = Vec::new();
    for index in 0..count as usize {
        let base = user_ptr::try_read_at::<u64>(iov, index * 2).map_err(|_| err(EFAULT))?;
        let len = user_ptr::try_read_at::<u64>(iov, index * 2 + 1).map_err(|_| err(EFAULT))?;
        if (len as i64) < 0 {
            return Err(err(EINVAL));
        }
        out.push((base, len));
    }
    Ok(out)
}

/// The non-empty segments of an iovec.
fn segments(iov: Vec<(u64, u64)>) -> impl Iterator<Item = (u64, u64)> {
    iov.into_iter().filter(|&(_, len)| len > 0)
}

/// `sendmsg(fd, msg, flags)`. A stream socket sends the segments in order
/// and stops at the first short or failed transfer (a short count is legal).
/// A message socket (datagram, seqpacket) needs the message in one piece, so
/// it takes at most one non-empty segment and refuses a scattered one with
/// `EOPNOTSUPP` instead of splitting the message.
pub(super) fn sys_sendmsg(fd: u64, msg: u64, flags: u64) -> u64 {
    let dont_wait = match send_flags(flags) {
        Ok(dont_wait) => dont_wait,
        Err(code) => return code,
    };
    let header = match read_msghdr(msg) {
        Ok(header) => header,
        Err(code) => return code,
    };
    if header.control != 0 && header.controllen != 0 {
        return err(EOPNOTSUPP);
    }
    let iov = match read_iov(header.iov, header.iovlen) {
        Ok(iov) => iov,
        Err(code) => return code,
    };
    let parts: Vec<(u64, u64)> = segments(iov).collect();
    let to = (header.name, header.namelen);
    if is_message_socket(fd) {
        return match parts.as_slice() {
            [] => super::socket::sys_sendto(fd, 0, 0, to, dont_wait),
            [(base, len)] => super::socket::sys_sendto(fd, *base, *len, to, dont_wait),
            _ => err(EOPNOTSUPP),
        };
    }
    let mut sent = 0u64;
    for (base, len) in parts {
        let n = super::socket::sys_sendto(fd, base, len, to, dont_wait);
        if (n as i64) < 0 {
            return if sent > 0 { sent } else { n };
        }
        sent += n;
        if n < len {
            break;
        }
    }
    sent
}

/// Whether `fd` keeps message boundaries (`SOCK_SEQPACKET`, `SOCK_DGRAM`).
fn is_message_socket(fd: u64) -> bool {
    task::fd_seqpacket(fd as usize) || super::inet::is_datagram(fd)
}

/// `recvmsg(fd, msg, flags)`: one receive scattered across every segment in
/// order (a message socket reads one message against their combined
/// capacity; `MSG_TRUNC` in `msg_flags` only when that message was longer).
/// The source address is written for datagram sockets that have one; the
/// control length comes back zero.
pub(super) fn sys_recvmsg(fd: u64, msg: u64, flags: u64) -> u64 {
    const MSG_TRUNC: i32 = 0x20;
    let (opts, wait_all) = match recv_flags(flags) {
        Ok(decoded) => decoded,
        Err(code) => return code,
    };
    let header = match read_msghdr(msg) {
        Ok(header) => header,
        Err(code) => return code,
    };
    let iov = match read_iov(header.iov, header.iovlen) {
        Ok(iov) => iov,
        Err(code) => return code,
    };
    let dest = Scatter::new(segments(iov));
    let from = if header.name != 0 {
        (header.name, msg + 8)
    } else {
        (0, 0)
    };
    let got = receive(fd, &dest, from, opts, wait_all);
    let n = got.result;
    if (n as i64) < 0 {
        return n;
    }
    let msg_flags = if got.truncated { MSG_TRUNC } else { 0 };
    if user_ptr::try_write::<u64>(msg + 40, 0).is_err()
        || user_ptr::try_write::<i32>(msg + 48, msg_flags).is_err()
    {
        return err(EFAULT);
    }
    n
}
