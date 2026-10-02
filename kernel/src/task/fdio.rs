//! File-descriptor I/O: stream reads/writes, poll, eventfds and dup (the
//! snapshot-file reads, writes and seeks are in `snapshot`).

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
            Fd::Inet { sock } => {
                let pair = sock.pair().ok_or(pipe::Error::BadEnd)?;
                let nonblock = pair.nonblock(Side::B);
                (Source::Socket(pair, Side::B), nonblock)
            }
            _ => return Err(pipe::Error::BadEnd),
        }
    };
    match source {
        Source::Pipe(pipe, end) => pipe.read(end, dst, nonblock),
        Source::Socket(pair, side) => pair.read(side, dst, nonblock),
    }
}

/// Whether a pipe or socket descriptor is in `O_NONBLOCK` mode; `None` when it
/// is not an open stream descriptor.
pub fn fd_stream_nonblock(fd: usize) -> Option<bool> {
    let tasks = TASKS.lock();
    let task = tasks[current()].as_ref()?;
    match task.fds.get(fd)? {
        Fd::Pipe { pipe, end } => Some(pipe.nonblock(*end)),
        Fd::Socket { pair, side } => Some(pair.nonblock(*side)),
        Fd::Inet { sock } => Some(sock.nonblock()),
        _ => None,
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
            Fd::Inet { sock } => {
                let pair = sock.pair().ok_or(pipe::Error::BadEnd)?;
                let nonblock = pair.nonblock(Side::B);
                (Sink::Socket(pair, Side::B), nonblock)
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
