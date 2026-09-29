//! File-descriptor I/O: stream reads/writes, peek/advance, seek and dup.

use super::*;

/// Read from a pipe end or socket side into kernel memory. The fd wrapper
/// resolves the shared object, drops the task-table lock, and then runs the
/// blocking read (which may park this task).
pub fn fd_stream_read(fd: usize, dst: &mut [u8]) -> Result<usize, pipe::Error> {
    enum Source {
        Pipe(Arc<Pipe>, End),
        Socket(Arc<SocketPair>, Side),
    }
    let (source, nonblock) = {
        let tasks = TASKS.lock();
        let task = tasks[current()].as_ref().ok_or(pipe::Error::BadEnd)?;
        if fd >= FD_COUNT {
            return Err(pipe::Error::BadEnd);
        }
        match &task.fds[fd] {
            Fd::Pipe { pipe, end } => (Source::Pipe(Arc::clone(pipe), *end), pipe.nonblock(*end)),
            Fd::Socket { pair, side } => (
                Source::Socket(Arc::clone(pair), *side),
                pair.nonblock(*side),
            ),
            _ => return Err(pipe::Error::BadEnd),
        }
    };
    match source {
        Source::Pipe(pipe, end) => pipe.read(end, dst, nonblock),
        Source::Socket(pair, side) => pair.read(side, dst, nonblock),
    }
}

/// Write to a pipe end or socket side from kernel memory.
pub fn fd_stream_write(fd: usize, src: &[u8]) -> Result<usize, pipe::Error> {
    enum Sink {
        Pipe(Arc<Pipe>, End),
        Socket(Arc<SocketPair>, Side),
    }
    let (sink, nonblock) = {
        let tasks = TASKS.lock();
        let task = tasks[current()].as_ref().ok_or(pipe::Error::BadEnd)?;
        if fd >= FD_COUNT {
            return Err(pipe::Error::BadEnd);
        }
        match &task.fds[fd] {
            Fd::Pipe { pipe, end } => (Sink::Pipe(Arc::clone(pipe), *end), pipe.nonblock(*end)),
            Fd::Socket { pair, side } => {
                (Sink::Socket(Arc::clone(pair), *side), pair.nonblock(*side))
            }
            _ => return Err(pipe::Error::BadEnd),
        }
    };
    match sink {
        Sink::Pipe(pipe, end) => pipe.write(src, end, nonblock),
        Sink::Socket(pair, side) => pair.write(side, src, nonblock),
    }
}

/// `poll` revents for a descriptor: `POLLIN`/`POLLOUT`/`POLLHUP` for streams,
/// terminal input readiness for fd 0. `None` means the slot is not open
/// (`POLLNVAL`).
pub fn fd_poll(fd: usize, events: u16) -> Option<u16> {
    // stdin's readiness comes from the input queue; `input_available` is the
    // one predicate for it (no table lock held here, so it can take its own).
    if fd == 0 && fd_kind(0) == FdKind::Terminal {
        return Some(Fd::Terminal.poll(events));
    }
    let target = fd_clone(fd)?;
    Some(target.poll(events))
}

/// Read the counter from an `eventfd` descriptor.
pub fn fd_eventfd_read(fd: usize) -> Result<u64, pipe::Error> {
    let target = fd_clone(fd).ok_or(pipe::Error::BadEnd)?;
    match &target {
        Fd::Event { event } => event.read(),
        _ => Err(pipe::Error::BadEnd),
    }
}

/// Add to an `eventfd` descriptor's counter.
pub fn fd_eventfd_write(fd: usize, value: u64) -> Result<(), pipe::Error> {
    let target = fd_clone(fd).ok_or(pipe::Error::BadEnd)?;
    match &target {
        Fd::Event { event } => event.write(value),
        _ => Err(pipe::Error::BadEnd),
    }
}

/// Whether a socket descriptor preserves message boundaries.
pub fn fd_seqpacket(fd: usize) -> bool {
    let tasks = TASKS.lock();
    let Some(task) = tasks[current()].as_ref() else {
        return false;
    };
    if fd >= FD_COUNT {
        return false;
    }
    matches!(&task.fds[fd], Fd::Socket { pair, .. } if pair.seqpacket())
}

/// Most bytes one [`fd_peek`] hands back; a short read is legal, so a huge
/// request is served in pieces instead of duplicating the whole file.
pub(super) const FD_READ_MAX: usize = 1 << 20;

/// The next up-to-`count` bytes of a file descriptor, *without* advancing its
/// offset. The bytes come back in a kernel buffer so the caller can copy them
/// to user memory through the validated path and only then [`fd_advance`].
///
/// This used to take a raw destination pointer and `copy_nonoverlapping` into it
/// while holding the task-table lock: a user-chosen kernel address was an
/// arbitrary kernel write with file-controlled contents.
pub fn fd_peek(fd: usize, count: usize) -> Option<Vec<u8>> {
    let tasks = TASKS.lock();
    let task = tasks[current()].as_ref()?;
    if fd >= FD_COUNT {
        return None;
    }
    if let Fd::File { data, offset } = &task.fds[fd] {
        if *offset >= data.len() {
            return Some(Vec::new()); // read at or past EOF
        }
        let remaining = data.len() - *offset;
        let n = remaining.min(count).min(FD_READ_MAX);
        Some(data[*offset..*offset + n].to_vec())
    } else {
        None
    }
}

/// Advance a file descriptor's offset by `n` bytes after a successful
/// [`fd_peek`] and copy-out.
pub fn fd_advance(fd: usize, n: usize) {
    let mut tasks = TASKS.lock();
    if let Some(task) = tasks[current()].as_mut() {
        if fd < FD_COUNT {
            if let Fd::File { offset, .. } = &mut task.fds[fd] {
                *offset = offset.saturating_add(n);
            }
        }
    }
}

/// Read up to `count` bytes from a file descriptor and advance its offset.
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // the tests read through it
pub fn fd_read(fd: usize, count: usize) -> Option<Vec<u8>> {
    let bytes = fd_peek(fd, count)?;
    fd_advance(fd, bytes.len());
    Some(bytes)
}

/// File size for a file descriptor (none for terminals/closed).
pub fn fd_size(fd: usize) -> Option<u64> {
    let tasks = TASKS.lock();
    match tasks[current()].as_ref() {
        Some(task) if fd < FD_COUNT => match &task.fds[fd] {
            Fd::File { data, .. } => Some(data.len() as u64),
            _ => None,
        },
        _ => None,
    }
}

/// The current read/write position of a file descriptor.
pub fn fd_offset(fd: usize) -> Option<usize> {
    let tasks = TASKS.lock();
    match tasks[current()].as_ref() {
        Some(task) if fd < FD_COUNT => match &task.fds[fd] {
            Fd::File { offset, .. } => Some(*offset),
            _ => None,
        },
        _ => None,
    }
}

/// Patch `data` into a file descriptor's snapshot at `offset`, extending (and
/// zero-filling) as needed, and advance the descriptor past the write. Returns
/// false unless the descriptor holds a regular file; the Linux ABI uses this
/// to make a writable fd read back its own writes after the backing file was
/// updated.
pub fn fd_apply_write(fd: usize, offset: usize, data: &[u8]) -> bool {
    let mut tasks = TASKS.lock();
    let Some(task) = tasks[current()].as_mut() else {
        return false;
    };
    if fd >= FD_COUNT {
        return false;
    }
    let Fd::File {
        data: buf,
        offset: pos,
    } = &mut task.fds[fd]
    else {
        return false;
    };
    if !snapshot::write_at(buf, offset, data) {
        return false;
    }
    *pos = offset + data.len(); // cannot overflow: `write_at` checked it
    true
}

/// Reposition a file descriptor (`whence`: 0=SET, 1=CUR, 2=END).
///
/// Returns `None` for an unknown `whence` or a signed position that would
/// overflow `i64` or be negative (Linux answers `-EINVAL` for both). The new
/// position may lie past end-of-file, as Linux allows for sparse writes; reads
/// there return zero bytes.
pub fn fd_seek(fd: usize, offset: i64, whence: u64) -> Option<u64> {
    let mut tasks = TASKS.lock();
    let task = tasks[current()].as_mut()?;
    if fd >= FD_COUNT {
        return None;
    }
    if let Fd::File { data, offset: pos } = &mut task.fds[fd] {
        let base = match whence {
            0 => 0i64,
            1 => i64::try_from(*pos).ok()?,
            2 => i64::try_from(data.len()).ok()?,
            _ => return None,
        };
        let new = base.checked_add(offset)?;
        if new < 0 {
            return None;
        }
        *pos = new as usize;
        Some(new as u64)
    } else {
        None
    }
}

/// Duplicate a descriptor into the lowest free slot at or above `min`.
/// `dup` uses `min = 3`; `F_DUPFD` passes the caller's argument.
pub fn fd_dup_min(fd: usize, min: usize) -> Option<usize> {
    let mut tasks = TASKS.lock();
    let task = tasks[current()].as_mut()?;
    if fd >= FD_COUNT {
        return None;
    }
    let entry = match &task.fds[fd] {
        Fd::Closed => return None,
        other => other.clone(),
    };
    for index in min.max(3)..FD_COUNT {
        if matches!(task.fds[index], Fd::Closed) {
            task.fds[index] = entry;
            // `dup`/`F_DUPFD` produce a descriptor without `FD_CLOEXEC`.
            task.fd_flags[index] = 0;
            return Some(index);
        }
    }
    None
}

/// Duplicate a descriptor into the lowest free slot (`dup(2)`).
pub fn fd_dup(fd: usize) -> Option<usize> {
    fd_dup_min(fd, 3)
}

/// Duplicate `old` into the specific descriptor `new` (closing it first).
/// `FD_CLOEXEC` is cleared on the new descriptor, as POSIX requires; an
/// `old == new` call is a no-op.
pub fn fd_dup2(old: usize, new: usize) -> Option<usize> {
    if old >= FD_COUNT || new >= FD_COUNT {
        return None;
    }
    if old == new {
        // Validating only: a closed `old` fails, an open one is unchanged.
        return match fd_kind(old) {
            FdKind::Closed => None,
            _ => Some(new),
        };
    }
    let replaced = {
        let mut tasks = TASKS.lock();
        let task = tasks[current()].as_mut()?;
        let entry = match &task.fds[old] {
            Fd::Closed => return None,
            other => other.clone(),
        };
        task.fd_flags[new] = 0;
        core::mem::replace(&mut task.fds[new], entry)
    };
    // The replaced descriptor may have been a pipe end; drop it unlocked.
    drop(replaced);
    Some(new)
}
