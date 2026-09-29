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
use super::time::millis_to_ticks;
use super::vfsfd;

/// Bytes staged per `read`/`write` call through a pipe. A short transfer is
/// legal on a pipe, so callers that want it all loop (as `write_all` does).
pub(super) const STREAM_CHUNK: usize = 4096;

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
        Some(task::ticks() + millis_to_ticks(timeout))
    };
    loop {
        let ready = scan_poll(fds, nfds);
        if ready > 0 {
            return ready;
        }
        match task::wait_poll(deadline) {
            WakeReason::Woken => {} // input arrived: rescan
            WakeReason::TimedOut => return 0,
            WakeReason::Interrupted => return err(EINTR),
        }
    }
}

/// One non-blocking poll pass over the user's `pollfd` array. Every open
/// descriptor kind is classified by the task layer, so pipes, sockets, files
/// and the terminal all report `POLLIN`/`POLLOUT`/`POLLHUP`/`POLLERR`/`POLLNVAL`.
fn scan_poll(fds: u64, nfds: u64) -> u64 {
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
        FdKind::Pipe | FdKind::Socket => write_stream(fd, ptr, len),
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
    crate::serial::write_bytes(bytes);
    // Answer a cursor-position report request (busybox line editing asks for it).
    if bytes.windows(4).any(|w| w == b"\x1b[6n") {
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
        return match task::fd_stream_write(fd as usize, &buf) {
            Ok(n) => n as u64,
            Err(pipe::Error::WouldBlock) => err(EAGAIN),
            Err(pipe::Error::BrokenPipe) => err(EPIPE),
            Err(pipe::Error::Interrupted) => err(EINTR),
            Err(pipe::Error::MessageTooLong) => err(EMSGSIZE),
            Err(pipe::Error::Invalid) => err(EINVAL),
            Err(pipe::Error::BadEnd) => err(EBADF),
        };
    }
    let want = (len as usize).min(STREAM_CHUNK);
    let mut buf = [0u8; STREAM_CHUNK];
    match user_ptr::try_bytes(ptr, want) {
        Ok(bytes) => buf[..want].copy_from_slice(bytes),
        Err(_) => return err(EFAULT),
    }
    match task::fd_stream_write(fd as usize, &buf[..want]) {
        Ok(n) => n as u64,
        Err(pipe::Error::WouldBlock) => err(EAGAIN),
        Err(pipe::Error::BrokenPipe) => err(EPIPE),
        Err(pipe::Error::Interrupted) => err(EINTR),
        Err(pipe::Error::MessageTooLong) => err(EMSGSIZE),
        Err(pipe::Error::Invalid) => err(EINVAL),
        Err(pipe::Error::BadEnd) => err(EBADF),
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
        FdKind::Terminal => read_terminal(ptr, len),
        FdKind::File => read_file_bytes(fd, ptr, len),
        FdKind::Vfs => vfsfd::with_file(fd, |file| vfsfd::read(file, ptr, len)),
        FdKind::Pipe | FdKind::Socket => read_stream(fd, ptr, len),
        FdKind::EventFd => read_eventfd(fd, ptr, len),
        FdKind::Unbound => err(ENOTCONN),
        FdKind::Closed | FdKind::Epoll | FdKind::Listener => err(EBADF),
    }
}

/// Pipe/socket read: block in the stream object until a chunk is available,
/// then copy it to the user buffer. `Ok(0)` (EOF) copies nothing.
///
/// A `SOCK_SEQPACKET` read stages only `min(len, capacity)` bytes, because the
/// stream layer truncates and discards the rest of an oversized message.
pub(super) fn read_stream(fd: u64, ptr: u64, len: u64) -> u64 {
    if len == 0 {
        return 0;
    }
    let seqpacket = task::fd_kind(fd as usize) == FdKind::Socket && task::fd_seqpacket(fd as usize);
    let want = if seqpacket {
        (len as usize).min(pipe::CAPACITY)
    } else {
        (len as usize).min(STREAM_CHUNK)
    };
    let mut heap = Vec::new();
    let mut stack = [0u8; STREAM_CHUNK];
    let buf: &mut [u8] = if want > STREAM_CHUNK {
        if heap.try_reserve_exact(want).is_err() {
            return err(ENOMEM);
        }
        heap.resize(want, 0);
        &mut heap
    } else {
        &mut stack[..want]
    };
    match task::fd_stream_read(fd as usize, buf) {
        Ok(n) => {
            if n > 0 {
                // Safety: the caller passes a valid user buffer of `len` bytes
                // (the syscall ABI's contract).
                unsafe { user_ptr::copy_to(ptr, &buf[..n]) };
            }
            n as u64
        }
        Err(pipe::Error::WouldBlock) => err(EAGAIN),
        Err(pipe::Error::BrokenPipe) => err(EPIPE),
        Err(pipe::Error::Interrupted) => err(EINTR),
        Err(pipe::Error::MessageTooLong) => err(EMSGSIZE),
        Err(pipe::Error::Invalid) => err(EINVAL),
        Err(pipe::Error::BadEnd) => err(EBADF),
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

/// Read terminal input as a byte stream: return once at least one key is
/// available (raw-mode programs read a byte at a time). Between checks the
/// task parks on the terminal wait queue, so an idle shell costs no CPU.
fn read_terminal(ptr: u64, len: u64) -> u64 {
    if len == 0 {
        return 0;
    }
    loop {
        if let Some(key) = task::take_key() {
            // Safety: destination within the user buffer (the syscall ABI's contract).
            unsafe { user_ptr::write::<u8>(ptr, key_to_byte(key)) };
            return 1;
        }
        // A key may have gone to another task's window; spurious wakeups just
        // loop. `read` has no timeout, so only an interrupt can end the wait.
        match task::wait_terminal() {
            WakeReason::Woken | WakeReason::TimedOut => {}
            WakeReason::Interrupted => return err(EINTR),
        }
    }
}

fn key_to_byte(key: crate::input::keyboard::Key) -> u8 {
    use crate::input::keyboard::Key;
    match key {
        Key::Char(c) => c as u8,
        Key::Enter => b'\n',
        Key::Space => b' ',
        Key::Tab => b'\t',
        Key::Backspace => 8,
        Key::Escape => 27,
        _ => 0,
    }
}
