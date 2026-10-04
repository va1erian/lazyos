//! `read`/`write` and `poll`, dispatched by descriptor kind
//! ([`crate::task::FdKind`]) to a small handler per kind: terminal,
//! pipe/socket stream, regular file, or `eventfd`. The vectored forms
//! (`readv`, `writev`, ...) are in [`super::iov`].

use alloc::vec::Vec;

use crate::ipc::pipe;
use crate::task::{self, FdKind, WakeReason};
use crate::user_ptr;

use super::errno::{err, EAGAIN, EBADF, EFAULT, EINTR, EINVAL, EMSGSIZE, ENOMEM, ENOTCONN, EPIPE};
use super::filerw::{read_file_bytes, write_file};
use super::scatter::{Received, Scatter};
use super::time::millis_deadline;
use super::vfsfd;

/// Bytes staged on the kernel stack per `read`/`write` call through a pipe
/// (and `sendfile`'s chunk).
pub(super) const STREAM_CHUNK: usize = 4096;
/// Most bytes one stream `read`/`write` moves: a whole `AF_INET` ring
/// (docs/performance-plan.md P4.4). Above [`STREAM_CHUNK`] the bytes are
/// staged on the heap, never borrowed across a block. A short transfer is
/// legal on a pipe, so callers that want it all loop (as `write_all` does).
const STREAM_MAX: usize = pipe::SMALL_CAPACITY;

/// A staging buffer of `want` bytes: on the stack up to [`STREAM_CHUNK`],
/// else on the heap (`None` when the heap cannot give it).
fn stage<'a>(
    stack: &'a mut [u8; STREAM_CHUNK],
    heap: &'a mut Vec<u8>,
    want: usize,
) -> Option<&'a mut [u8]> {
    if want <= STREAM_CHUNK {
        return Some(&mut stack[..want]);
    }
    heap.try_reserve_exact(want).ok()?;
    heap.resize(want, 0);
    Some(heap)
}

/// Most `pollfd` entries one `poll` may scan.
const POLL_MAX_FDS: u64 = 1024;

/// `poll(fds, nfds, timeout)`. Only stdin is pollable; park on the terminal
/// wait queue until it has input or the deadline passes (no busy loop).
pub(super) fn sys_poll(fds: u64, nfds: u64, timeout: u64) -> u64 {
    if nfds > POLL_MAX_FDS {
        return err(EINVAL);
    }
    if timeout == 0 {
        // Zero means "report readiness now" — never block.
        return scan_poll(fds, nfds);
    }
    // The timeout is an i32 count of milliseconds; negative blocks forever.
    let deadline = if (timeout as i64) < 0 {
        None
    } else {
        Some(millis_deadline(timeout))
    };
    loop {
        let ready = scan_poll(fds, nfds);
        if ready > 0 {
            return ready;
        }
        match task::wait_poll_ns(deadline) {
            WakeReason::Woken => {} // input arrived: rescan
            WakeReason::TimedOut => return 0,
            WakeReason::Interrupted => return err(EINTR),
        }
    }
}

/// One non-blocking poll pass over the user's `pollfd` array. Every open
/// descriptor kind is classified by the task layer, so pipes, sockets, files
/// and the terminal all report `POLLIN`/`POLLOUT`/`POLLHUP`/`POLLERR`/`POLLNVAL`.
pub(super) fn scan_poll(fds: u64, nfds: u64) -> u64 {
    const POLLNVAL: u16 = 0x0020;
    let mut ready = 0u64;
    for i in 0..nfds {
        // struct pollfd { i32 fd; i16 events; i16 revents; }
        // The whole 8-byte entry must fit below `u64::MAX`, so the field
        // offsets added below cannot wrap.
        let Some(entry) = i
            .checked_mul(8)
            .and_then(|off| fds.checked_add(off))
            .filter(|entry| entry.checked_add(8).is_some())
        else {
            return err(EFAULT);
        };
        let (Ok(fd), Ok(events)) = (
            user_ptr::try_read::<i32>(entry),
            user_ptr::try_read::<u16>(entry + 4),
        ) else {
            return err(EFAULT);
        };
        let revents = if fd < 0 {
            0
        } else {
            task::fd_poll(fd as usize, events).unwrap_or(POLLNVAL)
        };
        if revents != 0 {
            ready += 1;
        }
        if user_ptr::try_write::<u16>(entry + 6, revents).is_err() {
            return err(EFAULT);
        }
    }
    ready
}

pub(super) fn sys_write(fd: u64, ptr: u64, len: u64) -> u64 {
    match task::fd_kind(fd as usize) {
        FdKind::Terminal => write_terminal(ptr, len),
        FdKind::Pty => super::tty::write_pty(fd, ptr, len),
        FdKind::Pipe | FdKind::Socket => write_stream(fd, ptr, len),
        FdKind::Inet => super::inet::sys_write(fd, ptr, len),
        FdKind::File => write_file(fd, ptr, len, None),
        FdKind::Vfs => vfsfd::with_file(fd, |file| vfsfd::write(file, ptr, len)),
        FdKind::EventFd => write_eventfd(fd, ptr, len),
        FdKind::Unbound => err(ENOTCONN),
        FdKind::Closed | FdKind::Epoll | FdKind::Listener => err(EBADF),
    }
}

/// Terminal writes (`fd` 0/1/2 and `/dev/tty`): the task's console buffer and
/// the serial log, with the busybox cursor-position reply.
fn write_terminal(ptr: u64, len: u64) -> u64 {
    if len == 0 {
        return 0;
    }
    let Ok(bytes) = user_ptr::try_bytes(ptr, len as usize) else {
        return err(EFAULT);
    };
    task::write_output(bytes);
    // Answer a cursor-position report request (busybox line editing asks for it).
    let asks_position = bytes.windows(4).any(|w| w == b"\x1b[6n");
    // Last use of the user slice: the mirror may let interrupts in (and
    // other threads run) once it has queued the bytes.
    crate::serial::mirror(bytes);
    if asks_position {
        task::inject_input(b"\x1b[1;1R");
    }
    len
}

/// Pipe/socket write: stage a chunk of user bytes, then let the stream object
/// block or report `-EPIPE`/`-EAGAIN`. A short count is legal; the caller
/// (`write_all`, busybox's `full_write`) retries.
///
/// A `SOCK_SEQPACKET` socket sends the whole call as one message: the bytes are
/// staged in one heap buffer (up to the pipe capacity, larger is `-EMSGSIZE`)
/// so framing cannot be split.
pub(super) fn write_stream(fd: u64, ptr: u64, len: u64) -> u64 {
    write_stream_opts(fd, ptr, len, false)
}

/// [`write_stream`] that never blocks when `dont_wait` (`MSG_DONTWAIT`).
pub(super) fn write_stream_opts(fd: u64, ptr: u64, len: u64, dont_wait: bool) -> u64 {
    if len == 0 {
        return 0;
    }
    if task::fd_kind(fd as usize) == FdKind::Socket && task::fd_seqpacket(fd as usize) {
        if len > pipe::CAPACITY as u64 {
            return err(EMSGSIZE);
        }
        let want = len as usize;
        let mut buf = Vec::new();
        if buf.try_reserve_exact(want).is_err() {
            return err(ENOMEM);
        }
        buf.resize(want, 0);
        match user_ptr::try_bytes(ptr, want) {
            Ok(bytes) => buf.copy_from_slice(bytes),
            Err(_) => return err(EFAULT),
        }
        return match task::fd_stream_send(fd as usize, &buf, dont_wait) {
            Ok(n) => n as u64,
            Err(pipe::Error::WouldBlock) => err(EAGAIN),
            Err(pipe::Error::BrokenPipe) => err(EPIPE),
            Err(pipe::Error::Interrupted) => err(EINTR),
            Err(pipe::Error::MessageTooLong) => err(EMSGSIZE),
            Err(pipe::Error::Invalid) => err(EINVAL),
            Err(pipe::Error::BadEnd) => err(EBADF),
        };
    }
    let want = (len as usize).min(STREAM_MAX);
    let (mut stack, mut heap) = ([0u8; STREAM_CHUNK], Vec::new());
    let Some(buf) = stage(&mut stack, &mut heap, want) else {
        return err(ENOMEM);
    };
    match user_ptr::try_bytes(ptr, want) {
        Ok(bytes) => buf.copy_from_slice(bytes),
        Err(_) => return err(EFAULT),
    }
    match task::fd_stream_send(fd as usize, buf, dont_wait) {
        Ok(n) => n as u64,
        Err(error) => pipe_error(error),
    }
}

/// The errno for a failed stream call (`-errno`, as the ABI returns it).
pub(super) fn pipe_error(error: pipe::Error) -> u64 {
    match error {
        pipe::Error::WouldBlock => err(EAGAIN),
        pipe::Error::BrokenPipe => err(EPIPE),
        pipe::Error::Interrupted => err(EINTR),
        pipe::Error::MessageTooLong => err(EMSGSIZE),
        pipe::Error::Invalid => err(EINVAL),
        pipe::Error::BadEnd => err(EBADF),
    }
}

/// `eventfd` write: exactly one 8-byte little-endian value to add.
fn write_eventfd(fd: u64, ptr: u64, len: u64) -> u64 {
    if len != 8 {
        return err(EINVAL);
    }
    // Safety: the caller passes an 8-byte user buffer (the syscall ABI's contract).
    let value = unsafe { user_ptr::read::<u64>(ptr) };
    match task::fd_eventfd_write(fd as usize, value) {
        Ok(()) => 8,
        Err(pipe::Error::WouldBlock) => err(EAGAIN),
        Err(pipe::Error::Interrupted) => err(EINTR),
        Err(pipe::Error::Invalid) => err(EINVAL),
        Err(pipe::Error::BadEnd) => err(EBADF),
        Err(pipe::Error::BrokenPipe | pipe::Error::MessageTooLong) => err(EINVAL),
    }
}

pub(super) fn sys_read(fd: u64, ptr: u64, len: u64) -> u64 {
    match task::fd_kind(fd as usize) {
        FdKind::Terminal => super::tty::read_console(ptr, len),
        FdKind::Pty => super::tty::read_pty(fd, ptr, len),
        FdKind::File => read_file_bytes(fd, ptr, len),
        FdKind::Vfs => vfsfd::with_file(fd, |file| vfsfd::read(file, ptr, len)),
        FdKind::Pipe | FdKind::Socket => read_stream(fd, ptr, len),
        FdKind::Inet => super::inet::sys_read(fd, ptr, len),
        FdKind::EventFd => read_eventfd(fd, ptr, len),
        FdKind::Unbound => err(ENOTCONN),
        FdKind::Closed | FdKind::Epoll | FdKind::Listener => err(EBADF),
    }
}

/// Pipe/socket read: block in the stream object until a chunk is available,
/// then copy it to the user buffer. `Ok(0)` (EOF) copies nothing.
///
/// A `SOCK_SEQPACKET` read stages one byte more than the destination holds
/// (up to the capacity): the stream layer discards the rest of an oversized
/// message, and that extra byte is how a truncation is told from an exact fit.
pub(super) fn read_stream(fd: u64, ptr: u64, len: u64) -> u64 {
    // `read` of nothing takes nothing, not even a message (Linux's
    // `sock_read_iter`); `recv` of nothing still takes one (below).
    if len == 0 {
        return 0;
    }
    read_stream_opts(fd, ptr, len, task::RecvOpts::default())
}

/// [`read_stream`] with `recv` modifiers (`MSG_DONTWAIT`, `MSG_PEEK`).
pub(super) fn read_stream_opts(fd: u64, ptr: u64, len: u64, opts: task::RecvOpts) -> u64 {
    recv_stream(fd, &Scatter::one(ptr, len), opts).result
}

/// [`read_stream_opts`] into any destination, reporting a truncated message.
pub(super) fn recv_stream(fd: u64, dest: &Scatter, opts: task::RecvOpts) -> Received {
    let len = dest.len();
    let seqpacket = task::fd_kind(fd as usize) == FdKind::Socket && task::fd_seqpacket(fd as usize);
    // A stream has nothing to give an empty buffer; a message socket still
    // takes one message, reported truncated (as `recv`/`recvmsg` do on Linux).
    if len == 0 && !seqpacket {
        return Received::of(0);
    }
    let len = usize::try_from(len).unwrap_or(usize::MAX);
    let want = if seqpacket {
        len.saturating_add(1).min(pipe::CAPACITY)
    } else {
        len.min(STREAM_MAX)
    };
    let (mut stack, mut heap) = ([0u8; STREAM_CHUNK], Vec::new());
    let Some(buf) = stage(&mut stack, &mut heap, want) else {
        return Received::of(err(ENOMEM));
    };
    let n = match task::fd_stream_recv(fd as usize, buf, opts) {
        Ok(n) => n,
        Err(pipe::Error::WouldBlock) => return Received::of(err(EAGAIN)),
        Err(pipe::Error::BrokenPipe) => return Received::of(err(EPIPE)),
        Err(pipe::Error::Interrupted) => return Received::of(err(EINTR)),
        Err(pipe::Error::MessageTooLong) => return Received::of(err(EMSGSIZE)),
        Err(pipe::Error::Invalid) => return Received::of(err(EINVAL)),
        Err(pipe::Error::BadEnd) => return Received::of(err(EBADF)),
    };
    let kept = n.min(len);
    if let Err(code) = dest.copy_out(&buf[..kept]) {
        return Received::of(code);
    }
    Received {
        result: kept as u64,
        truncated: n > kept,
    }
}

/// `eventfd` read: exactly one 8-byte little-endian value. A shorter count is
/// `EINVAL`, as Linux reports.
fn read_eventfd(fd: u64, ptr: u64, len: u64) -> u64 {
    if len < 8 {
        return err(EINVAL);
    }
    match task::fd_eventfd_read(fd as usize) {
        Ok(value) => {
            // Safety: the caller passes a user buffer of at least 8 bytes (the
            // syscall ABI's contract).
            unsafe { user_ptr::write::<u64>(ptr, value) };
            8
        }
        Err(pipe::Error::WouldBlock) => err(EAGAIN),
        Err(pipe::Error::Interrupted) => err(EINTR),
        Err(pipe::Error::BadEnd) => err(EBADF),
        Err(pipe::Error::Invalid | pipe::Error::BrokenPipe | pipe::Error::MessageTooLong) => {
            err(EINVAL)
        }
    }
}
