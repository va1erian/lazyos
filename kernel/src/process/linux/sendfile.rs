//! `sendfile(2)`: kernel-side copy between descriptors for BusyBox `cat`.

use crate::ipc::pipe;
use crate::task::{self, FdKind};

use super::errno::{err, EAGAIN, EBADF, EINTR, EINVAL, EMSGSIZE, EPIPE};
use super::io::STREAM_CHUNK;
use super::iov::{add_iov_total, partial_or};

/// `sendfile(out_fd, in_fd, offset, count)`: copy `count` bytes from `in_fd` to
/// `out_fd` without a user buffer in between.
///
/// BusyBox `cat` uses the offset-less form (a NULL `offset` pointer), so a
/// non-NULL offset is `-EINVAL` rather than a silent misread. The source may be
/// a regular file or a pipe/socket; the destination must be a terminal or a
/// blocking stream (a non-blocking one is `-EINVAL`, see below), because a regular-file destination needs the copy-up write path and
/// is not something `cat` asks for. A short read at EOF ends the transfer early; a short
/// write is retried until the whole chunk is delivered, and a destination that
/// errors or accepts nothing ends it with the byte count done so far.
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
    // A source byte, once read, cannot be pushed back, and a non-blocking
    // stream can refuse the tail of a chunk mid-copy, which would lose it. Such
    // a destination is refused up front (`EINVAL`), so the caller (BusyBox
    // `cat`) falls back to its own read/write loop, which handles `EAGAIN`.
    if task::fd_stream_nonblock(out_fd as usize) == Some(true) {
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
                                // The bytes are already consumed from the source, so a short write must
                                // not end the call: keep writing the suffix until the destination has
                                // accepted the whole chunk. Only an error (or a stalled destination that
                                // accepts nothing) stops early, reporting what was really delivered.
        let mut done = 0usize;
        while done < got {
            let written = write_kernel_bytes(out_fd, &buf[done..got]);
            if written > (got - done) as u64 {
                return partial_or(total, written); // encoded error
            }
            if written == 0 {
                return total; // destination accepts nothing: report progress
            }
            done += written as usize;
            total = match add_iov_total(total, written) {
                Ok(sum) => sum,
                Err(code) => return partial_or(total, code),
            };
        }
        if short {
            break; // input drained: a short transfer ends the call
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
