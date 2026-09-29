//! `read`/`write` and the syscalls built on them (`readv`/`writev`, `poll`),
//! dispatched by descriptor kind ([`crate::task::FdKind`]) to a small handler
//! per kind: terminal, pipe/socket stream, regular file, or `eventfd`.

use alloc::vec::Vec;

use crate::fs::vfs::Id;
use crate::ipc::pipe;
use crate::task::{self, FdKind, WakeReason};
use crate::user_ptr;

use super::errno::{
    err, fs_err, EAGAIN, EBADF, EFAULT, EINTR, EINVAL, EMSGSIZE, ENOMEM, ENOTCONN, EPIPE,
};
use super::fd::{fd_meta_get, fd_meta_sync_len};
use super::time::millis_to_ticks;

/// Bytes staged per `read`/`write` call through a pipe. A short transfer is
/// legal on a pipe, so callers that want it all loop (as `write_all` does).
const STREAM_CHUNK: usize = 4096;

/// Most `iovec` entries one `readv`/`writev` may name (Linux `UIO_MAXIOV`).
/// Syscalls run with interrupts off, so an unbounded user count would freeze
/// the machine.
const UIO_MAXIOV: u64 = 1024;

/// Most `pollfd` entries one `poll` may scan.
const POLL_MAX_FDS: u64 = 1024;

/// Read entry `index` of a user `iovec` array as `(base, len)`. A bad array is
/// `-EFAULT` rather than a run of zero-length entries.
fn iovec_at(iov: u64, index: u64) -> Result<(u64, u64), u64> {
    let entry = iov
        .checked_add(index.checked_mul(16).ok_or(err(EFAULT))?)
        .ok_or(err(EFAULT))?;
    let base = user_ptr::try_read::<u64>(entry).map_err(|_| err(EFAULT))?;
    let len_at = entry.checked_add(8).ok_or(err(EFAULT))?;
    let len = user_ptr::try_read::<u64>(len_at).map_err(|_| err(EFAULT))?;
    // A length past `isize::MAX` is `-EINVAL` (as on Linux); it also keeps a
    // byte count distinguishable from an encoded `-errno` below.
    if len > i64::MAX as u64 {
        return Err(err(EINVAL));
    }
    Ok((base, len))
}

/// The result of a vectored transfer that failed with `code` after `total`
/// bytes moved: report the bytes done so a retry cannot repeat them.
fn partial_or(total: u64, code: u64) -> u64 {
    if total > 0 {
        total
    } else {
        code
    }
}

/// Add a segment length to a running `readv`/`writev` total; a total that
/// overflows `isize` is `-EINVAL`, as on Linux.
fn add_iov_total(total: u64, part: u64) -> Result<u64, u64> {
    match total.checked_add(part) {
        Some(sum) if sum <= i64::MAX as u64 => Ok(sum),
        _ => Err(err(EINVAL)),
    }
}

/// `writev(fd, iov, iovcnt)`: `struct iovec { void *base; size_t len; }`.
pub(super) fn sys_writev(fd: u64, iov: u64, count: u64) -> u64 {
    if count > UIO_MAXIOV {
        return err(EINVAL);
    }
    let mut total = 0u64;
    for i in 0..count {
        let (base, len) = match iovec_at(iov, i) {
            Ok(entry) => entry,
            Err(code) => return partial_or(total, code),
        };
        let written = sys_write(fd, base, len);
        if written > len {
            return partial_or(total, written); // error
        }
        total = match add_iov_total(total, written) {
            Ok(sum) => sum,
            Err(code) => return code,
        };
    }
    total
}

/// `readv(fd, iov, iovcnt)`.
pub(super) fn sys_readv(fd: u64, iov: u64, count: u64) -> u64 {
    if count > UIO_MAXIOV {
        return err(EINVAL);
    }
    let mut total = 0u64;
    for i in 0..count {
        let (base, len) = match iovec_at(iov, i) {
            Ok(entry) => entry,
            Err(code) => return partial_or(total, code),
        };
        let got = sys_read(fd, base, len);
        if got > len {
            return partial_or(total, got); // error
        }
        total = match add_iov_total(total, got) {
            Ok(sum) => sum,
            Err(code) => return code,
        };
        if got < len {
            break; // short read: stop
        }
    }
    total
}

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
        FdKind::File => write_file(fd, ptr, len),
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

/// Write through a regular-file descriptor: the ABI VFS updates the backing
/// file, then the fd's snapshot is patched so the same descriptor reads back
/// its own writes. `O_APPEND` descriptors ignore the position and write at the
/// current EOF.
fn write_file(fd: u64, ptr: u64, len: u64) -> u64 {
    if len == 0 {
        return 0;
    }
    let Some(meta) = fd_meta_get(fd as usize) else {
        return err(EBADF);
    };
    if meta.device {
        return len; // /dev/null and friends discard the bytes
    }
    if !meta.writable {
        return err(EBADF);
    }
    let Some(path) = meta.path else {
        return err(EBADF);
    };
    let Ok(bytes) = user_ptr::try_bytes(ptr, len as usize) else {
        return err(EFAULT);
    };
    let id = Id::current();
    let offset = if meta.append {
        match crate::fs::abi_stat(id, &path) {
            Ok(stat) => stat.size,
            Err(error) => return fs_err(error),
        }
    } else {
        task::fd_offset(fd as usize).unwrap_or(0) as u64
    };
    // Get the descriptor's snapshot ready first: once the backing file has
    // accepted the bytes, mirroring them must not be able to fail.
    if !task::prepare_fd_write(fd as usize, offset as usize, bytes.len()) {
        return err(ENOMEM);
    }
    match crate::fs::abi_write(id, &path, offset, bytes) {
        Ok(written) => {
            if !task::fd_apply_write(fd as usize, offset as usize, &bytes[..written]) {
                return err(ENOMEM);
            }
            fd_meta_sync_len(fd as usize);
            written as u64
        }
        Err(error) => fs_err(error),
    }
}

pub(super) fn sys_read(fd: u64, ptr: u64, len: u64) -> u64 {
    match task::fd_kind(fd as usize) {
        FdKind::Terminal => read_terminal(ptr, len),
        FdKind::File => read_file_bytes(fd, ptr, len),
        FdKind::Pipe | FdKind::Socket => read_stream(fd, ptr, len),
        FdKind::EventFd => read_eventfd(fd, ptr, len),
        FdKind::Unbound => err(ENOTCONN),
        FdKind::Closed | FdKind::Epoll | FdKind::Listener => err(EBADF),
    }
}

/// Read from a regular-file descriptor into the user buffer at `ptr`.
///
/// The bytes are staged in kernel memory, copied out through the validated
/// path, and the descriptor's offset only advances once the copy succeeded, so
/// a bad buffer is `-EFAULT` and loses nothing.
pub(super) fn read_file_bytes(fd: u64, ptr: u64, len: u64) -> u64 {
    let Some(chunk) = task::fd_peek(fd as usize, len as usize) else {
        return 0;
    };
    if user_ptr::try_copy_to(ptr, &chunk).is_err() {
        return err(EFAULT);
    }
    task::fd_advance(fd as usize, chunk.len());
    chunk.len() as u64
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

/// `sendfile(out_fd, in_fd, offset, count)`: copy `count` bytes from `in_fd` to
/// `out_fd` without a user buffer in between.
///
/// BusyBox `cat` uses the offset-less form (a NULL `offset` pointer), so a
/// non-NULL offset is `-EINVAL` rather than a silent misread. The source may be
/// a regular file or a pipe/socket; the destination must be a terminal or a
/// stream, because a regular-file destination needs the copy-up write path and
/// is not something `cat` asks for. A short read at EOF, or a short write to a
/// full pipe, ends the transfer early with the byte count done so far.
pub(super) fn sys_sendfile(out_fd: u64, in_fd: u64, offset: u64, count: u64) -> u64 {
    if count == 0 {
        return 0;
    }
    // BusyBox's `cat` passes NULL for the offset (the descriptor's own position
    // is used). The positional form would need a pread-like path; reject it
    // explicitly so a wrong answer is impossible.
    if offset != 0 {
        return err(EINVAL);
    }
    let source = task::fd_kind(in_fd as usize);
    if !matches!(source, FdKind::File | FdKind::Pipe | FdKind::Socket) {
        return err(EINVAL);
    }
    if !matches!(
        task::fd_kind(out_fd as usize),
        FdKind::Terminal | FdKind::Pipe | FdKind::Socket
    ) {
        return err(EINVAL);
    }
    let mut buf = [0u8; STREAM_CHUNK];
    let mut total = 0u64;
    while total < count {
        let want = ((count - total) as usize).min(STREAM_CHUNK);
        let got: usize = if source == FdKind::File {
            // `fd_read` advances the offset; an empty read is EOF.
            match task::fd_read(in_fd as usize, want) {
                Some(chunk) => {
                    let n = chunk.len().min(want);
                    buf[..n].copy_from_slice(&chunk[..n]);
                    n
                }
                None => return err(EBADF),
            }
        } else {
            match task::fd_stream_read(in_fd as usize, &mut buf[..want]) {
                Ok(n) => n,
                Err(error) => return partial_or(total, stream_error(error)),
            }
        };
        if got == 0 {
            break; // EOF
        }
        let short = got < want; // a short stream read means nothing more is queued
        let written = write_kernel_bytes(out_fd, &buf[..got]);
        if written > got as u64 {
            return partial_or(total, written); // encoded error
        }
        total = match add_iov_total(total, written) {
            Ok(sum) => sum,
            Err(code) => return partial_or(total, code),
        };
        if written < got as u64 || short {
            break; // pipe full or input drained: a short transfer ends the call
        }
    }
    total
}

/// Map a pipe/socket error to its errno (shared by `sendfile`).
fn stream_error(error: pipe::Error) -> u64 {
    match error {
        pipe::Error::WouldBlock => err(EAGAIN),
        pipe::Error::BrokenPipe => err(EPIPE),
        pipe::Error::Interrupted => err(EINTR),
        pipe::Error::MessageTooLong => err(EMSGSIZE),
        pipe::Error::Invalid => err(EINVAL),
        pipe::Error::BadEnd => err(EBADF),
    }
}

/// Write an already-in-kernel slice to a terminal or stream descriptor; a
/// regular-file destination is `-EINVAL` (see [`sys_sendfile`]). The return is
/// a byte count, or an encoded `-errno` larger than any count.
fn write_kernel_bytes(fd: u64, bytes: &[u8]) -> u64 {
    match task::fd_kind(fd as usize) {
        FdKind::Terminal => {
            task::write_output(bytes);
            crate::serial::write_bytes(bytes);
            bytes.len() as u64
        }
        FdKind::Pipe | FdKind::Socket => match task::fd_stream_write(fd as usize, bytes) {
            Ok(n) => n as u64,
            Err(error) => stream_error(error),
        },
        _ => err(EINVAL),
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
