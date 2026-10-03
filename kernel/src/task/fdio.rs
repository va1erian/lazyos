//! File-descriptor I/O: stream reads/writes, poll, eventfds and dup (the
//! snapshot-file reads, writes and seeks are in `snapshot`).

use super::*;

/// Read from a pipe end or socket side into kernel memory. The fd wrapper
/// resolves the shared object, drops the task-table lock, and then runs the
/// blocking read (which may park this task).
pub fn fd_stream_read(fd: usize, dst: &mut [u8]) -> Result<usize, pipe::Error> {
    fd_stream_recv(fd, dst, RecvOpts::default())
}

/// Per-call modifiers of a stream read (`recv` flags).
#[derive(Clone, Copy, Default, Debug)]
pub struct RecvOpts {
    /// `MSG_DONTWAIT`: never block, whatever the descriptor's `O_NONBLOCK`.
    pub dont_wait: bool,
    /// `MSG_PEEK`: leave the bytes queued.
    pub peek: bool,
}

/// [`fd_stream_read`] with `recv` modifiers.
pub fn fd_stream_recv(fd: usize, dst: &mut [u8], opts: RecvOpts) -> Result<usize, pipe::Error> {
    enum Source {
        Pipe(Arc<Pipe>, End),
        Socket(Arc<SocketPair>, Side),
    }
    let (source, nonblock) = {
        let tasks = TASKS.lock();
        let task = tasks[current()].as_ref().ok_or(pipe::Error::BadEnd)?;
        match task.fds.get(fd).ok_or(pipe::Error::BadEnd)? {
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
    let nonblock = nonblock || opts.dont_wait;
    match (source, opts.peek) {
        (Source::Pipe(pipe, end), false) => pipe.read(end, dst, nonblock),
        (Source::Pipe(pipe, end), true) => pipe.peek(end, dst, nonblock),
        (Source::Socket(pair, side), false) => pair.read(side, dst, nonblock),
        (Source::Socket(pair, side), true) => pair.peek(side, dst, nonblock),
    }
}

/// Bytes queued for reading on a pipe or socket descriptor (`FIONREAD`).
pub fn fd_stream_queued(fd: usize) -> Option<usize> {
    match fd_clone(fd)? {
        Fd::Pipe {
            ref pipe,
            end: End::Read,
        } => Some(pipe.queued()),
        Fd::Socket { ref pair, side } => Some(pair.queued(side)),
        Fd::Inet { ref sock } => sock.pair().map(|pair| pair.queued(Side::B)),
        _ => None,
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
    fd_stream_send(fd, src, false)
}

/// [`fd_stream_write`] that never blocks when `dont_wait` (`MSG_DONTWAIT`).
pub fn fd_stream_send(fd: usize, src: &[u8], dont_wait: bool) -> Result<usize, pipe::Error> {
    enum Sink {
        Pipe(Arc<Pipe>, End),
        Socket(Arc<SocketPair>, Side),
    }
    let (sink, nonblock) = {
        let tasks = TASKS.lock();
        let task = tasks[current()].as_ref().ok_or(pipe::Error::BadEnd)?;
        match task.fds.get(fd).ok_or(pipe::Error::BadEnd)? {
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
    let nonblock = nonblock || dont_wait;
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
    if fd_kind(fd) == FdKind::Terminal {
        let mut revents = events & pipe::POLLOUT;
        if events & pipe::POLLIN != 0 && consoletty::console_readable() {
            revents |= pipe::POLLIN;
        }
        return Some(revents);
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
    matches!(task.fds.get(fd), Some(Fd::Socket { pair, .. }) if pair.seqpacket())
}

/// Duplicate a descriptor into the lowest free slot at or above `min`.
/// `dup` uses `min = 3`; `F_DUPFD` passes the caller's argument.
/// `None` when `fd` is not open or no slot is free below `limit.fd_max`.
pub fn fd_dup_min(fd: usize, min: usize) -> Option<usize> {
    let mut junk = Vec::new();
    let refused = {
        let mut tasks = TASKS.lock();
        let me = current();
        let task = tasks[me].as_mut()?;
        let entry = task.fds.get(fd)?.clone();
        // `dup`/`F_DUPFD` produce a descriptor without `FD_CLOEXEC`.
        match task.fds.install_lowest(min.max(3), entry) {
            Ok(index) => {
                fdshare::mirror_fd(&mut tasks, me, index, &mut junk);
                drop(tasks);
                drop(junk);
                return Some(index);
            }
            Err(entry) => entry,
        }
    };
    // The refused copy holds a pipe reference: drop it unlocked.
    drop(refused);
    None
}

/// Duplicate a descriptor into the lowest free slot (`dup(2)`).
pub fn fd_dup(fd: usize) -> Option<usize> {
    fd_dup_min(fd, 3)
}

/// Duplicate `old` into the specific descriptor `new` (closing it first).
/// `FD_CLOEXEC` is cleared on the new descriptor, as POSIX requires; an
/// `old == new` call is a no-op.
/// `None` when `old` is not open or `new` is at or past `limit.fd_max`.
pub fn fd_dup2(old: usize, new: usize) -> Option<usize> {
    if new >= fd_max() {
        return None;
    }
    if old == new {
        // Validating only: a closed `old` fails, an open one is unchanged.
        return match fd_kind(old) {
            FdKind::Closed => None,
            _ => Some(new),
        };
    }
    let mut junk = Vec::new();
    let (replaced, result) = {
        let mut tasks = TASKS.lock();
        let me = current();
        let task = tasks[me].as_mut()?;
        let entry = task.fds.get(old)?.clone();
        match task.fds.put(new, entry) {
            Ok(old_entry) => {
                fdshare::mirror_fd(&mut tasks, me, new, &mut junk);
                (old_entry, Some(new))
            }
            Err(refused) => (refused, None),
        }
    };
    // The replaced (or refused) descriptor may be a pipe end; drop it unlocked.
    drop(replaced);
    drop(junk);
    result
}

/// Give a child the caller's descriptors as its standard streams (`spawnv`'s
/// `STDIO` request, issue #529): `map[i]` is the caller's descriptor that
/// becomes the child's descriptor `i`, or `None` to leave the terminal there.
/// Nothing else is shared, so a sandboxed child sees only the pipes it was
/// handed. Returns `false`, changing nothing, when a named descriptor is out
/// of range or closed or either task is gone.
///
/// Called by the spawning syscall before the child first runs (interrupts are
/// off in the gate), so no descriptor of the child is in use yet.
pub fn give_stdio(child: usize, map: &[Option<usize>; 3]) -> bool {
    let mut tasks = TASKS.lock();
    let parent = current();
    let Some(source) = tasks[parent].as_ref() else {
        return false;
    };
    // Validate every source first, so a refusal clones (and so retains)
    // nothing while the table is locked.
    let open = |fd: &Option<usize>| fd.is_none_or(|fd| source.fds.is_open(fd));
    if !map.iter().all(open) || tasks[child].is_none() {
        return false;
    }
    let entries: [Fd; 3] = core::array::from_fn(|slot| {
        map[slot]
            .and_then(|fd| source.fds.get(fd).cloned())
            .unwrap_or(Fd::Terminal)
    });
    let replaced: [Fd; 3] = match tasks[child].as_mut() {
        Some(target) => {
            let mut entries = entries.into_iter();
            core::array::from_fn(|slot| {
                let entry = entries.next().unwrap_or(Fd::Terminal);
                // `put` clears the slot's flags. Slots 0-2 always fit (fd_max is
                // at least 64); a refusal hands the entry back to drop unlocked.
                target
                    .fds
                    .put(slot, entry)
                    .unwrap_or_else(|refused| refused)
            })
        }
        None => entries,
    };
    drop(tasks);
    // The replaced entries may hold pipe references; they drop here, with the
    // table unlocked (queue-before-table lock order).
    drop(replaced);
    true
}
