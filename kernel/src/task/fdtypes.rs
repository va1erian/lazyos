//! Per-task file-descriptor table entries and their clone/drop semantics.

use super::*;
use crate::fs::openfile::OpenFile;

/// Number of file descriptors per task.
pub const FD_COUNT: usize = 16;

/// Per-descriptor `FD_CLOEXEC` bit in [`Task::fd_flags`].
pub const FD_CLOEXEC: u16 = 1;

/// The socket type an unbound `socket(2)` descriptor carries to `connect`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SocketKind {
    Stream,
    Seqpacket,
}

/// A Linux file descriptor slot.
pub enum Fd {
    /// Unused slot.
    Closed,
    /// stdin/stdout/stderr (and `/dev/tty`): the task's own terminal.
    Terminal,
    /// A regular file whose contents were read at open time: one open file
    /// description (snapshot, offset and the open's metadata) that
    /// `dup`/`fork`/`execve` share.
    File { file: Arc<SnapFile> },
    /// A regular file on the persistent mount, read and written in place
    /// through the VFS. `dup`/`fork` share the one open file description (and
    /// with it the offset), as POSIX requires.
    Vfs { file: Arc<OpenFile> },
    /// One end of an anonymous pipe (`pipe`/`pipe2`).
    Pipe { pipe: Arc<Pipe>, end: End },
    /// One side of an `AF_UNIX` socket pair (`socketpair` or an accepted
    /// pathname connection).
    Socket { pair: Arc<SocketPair>, side: Side },
    /// An `eventfd` counter.
    Event { event: Arc<EventFd> },
    /// An `epoll` instance.
    Epoll { epoll: Arc<Epoll> },
    /// A bound, listening `AF_UNIX` socket.
    UnixListener { listener: Arc<Listener> },
    /// A socket created by `socket(2)` but not yet bound or connected.
    Unbound { kind: SocketKind, nonblock: bool },
    /// An `AF_INET` socket, served by `netd` (`crate::ipc::inet`). Once it has
    /// a connection its data path is a socket pair, driven like `Socket`.
    Inet { sock: Arc<InetSock> },
}

impl Fd {
    /// A pipe end, taking the pipe's reader/writer reference.
    pub fn pipe_end(pipe: Arc<Pipe>, end: End) -> Fd {
        pipe.acquire(end);
        Fd::Pipe { pipe, end }
    }

    /// A socket side, taking the side's reference (and its directions' on the
    /// first open).
    pub fn socket_side(pair: Arc<SocketPair>, side: Side) -> Fd {
        pair.acquire(side);
        Fd::Socket { pair, side }
    }

    /// A socket side whose reference the caller already holds (a pending
    /// connection handed over by `Listener::take_pending`); nothing new is
    /// acquired, and dropping the `Fd` releases it.
    pub fn socket_side_adopt(pair: Arc<SocketPair>, side: Side) -> Fd {
        Fd::Socket { pair, side }
    }

    /// `poll` revents for this descriptor. `events` are `POLL*` bits; closed
    /// slots report nothing (callers map them to `POLLNVAL`).
    pub fn poll(&self, events: u16) -> u16 {
        self.poll_gen(events).0
    }

    /// [`poll`](Fd::poll) plus the handle's freshness counter, used by
    /// edge-triggered `epoll` interests.
    pub fn poll_gen(&self, events: u16) -> (u16, u64) {
        match self {
            Fd::Closed => (0, 0),
            Fd::Terminal => {
                // stdin's readiness is the shared input queue's; other
                // terminal descriptors are output-only.
                let mut revents = 0;
                if events & pipe::POLLIN != 0 && input_available() {
                    revents |= pipe::POLLIN;
                }
                if events & pipe::POLLOUT != 0 {
                    revents |= pipe::POLLOUT;
                }
                (revents, crate::task::input_gen())
            }
            Fd::File { .. } => {
                if events & pipe::POLLIN != 0 {
                    (pipe::POLLIN, 0)
                } else {
                    (0, 0)
                }
            }
            Fd::Vfs { file } => {
                // A regular file never blocks: ready whenever it is open in
                // the direction asked for.
                let mut revents = 0;
                if events & pipe::POLLIN != 0 && file.readable() {
                    revents |= pipe::POLLIN;
                }
                if events & pipe::POLLOUT != 0 && file.writable() {
                    revents |= pipe::POLLOUT;
                }
                (revents, 0)
            }
            Fd::Pipe { pipe, end } => pipe.poll_gen(*end, events),
            Fd::Socket { pair, side } => pair.poll_gen(*side, events),
            Fd::Event { event } => event.poll_gen(events),
            Fd::Epoll { epoll } => {
                if events & pipe::POLLIN != 0 && epoll.has_ready() {
                    (pipe::POLLIN, 0)
                } else {
                    (0, 0)
                }
            }
            Fd::UnixListener { listener } => listener.poll_gen(events),
            Fd::Unbound { .. } => (0, 0),
            Fd::Inet { sock } => sock.poll_gen(events),
        }
    }
}

/// Clone plus retain: `dup` and `fork` share the same pipe, so the reference
/// counts must follow the new descriptor.
impl Clone for Fd {
    fn clone(&self) -> Self {
        match self {
            Fd::Closed => Fd::Closed,
            Fd::Terminal => Fd::Terminal,
            Fd::File { file } => Fd::File {
                file: Arc::clone(file),
            },
            Fd::Vfs { file } => Fd::Vfs {
                file: Arc::clone(file),
            },
            Fd::Pipe { pipe, end } => Fd::pipe_end(Arc::clone(pipe), *end),
            Fd::Socket { pair, side } => Fd::socket_side(Arc::clone(pair), *side),
            Fd::Event { event } => Fd::Event {
                event: Arc::clone(event),
            },
            Fd::Epoll { epoll } => Fd::Epoll {
                epoll: Arc::clone(epoll),
            },
            Fd::UnixListener { listener } => Fd::UnixListener {
                listener: Arc::clone(listener),
            },
            Fd::Unbound { kind, nonblock } => Fd::Unbound {
                kind: *kind,
                nonblock: *nonblock,
            },
            Fd::Inet { sock } => Fd::Inet {
                sock: Arc::clone(sock),
            },
        }
    }
}

impl Fd {
    /// Whether two descriptors name the same open file (what `dup` and `fork`
    /// share). A socket not yet bound has no shared object, so it never
    /// matches.
    pub fn same_file(&self, other: &Fd) -> bool {
        match (self, other) {
            (Fd::Terminal, Fd::Terminal) => true,
            (Fd::File { file: a }, Fd::File { file: b }) => Arc::ptr_eq(a, b),
            (Fd::Vfs { file: a }, Fd::Vfs { file: b }) => Arc::ptr_eq(a, b),
            (Fd::Pipe { pipe: a, end: x }, Fd::Pipe { pipe: b, end: y }) => {
                Arc::ptr_eq(a, b) && x == y
            }
            (Fd::Socket { pair: a, side: x }, Fd::Socket { pair: b, side: y }) => {
                Arc::ptr_eq(a, b) && x == y
            }
            (Fd::Event { event: a }, Fd::Event { event: b }) => Arc::ptr_eq(a, b),
            (Fd::Epoll { epoll: a }, Fd::Epoll { epoll: b }) => Arc::ptr_eq(a, b),
            (Fd::UnixListener { listener: a }, Fd::UnixListener { listener: b }) => {
                Arc::ptr_eq(a, b)
            }
            (Fd::Inet { sock: a }, Fd::Inet { sock: b }) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// Releasing a descriptor drops its reference. This fires on `close`, on
/// `dup2` replacing a slot, and when an exited task's table is dropped; the
/// drop must happen with the task table unlocked because the last reference
/// wakes a wait queue (queue-before-table lock order). The fd helpers below
/// take the old entry out under the lock and drop it after releasing it.
impl Drop for Fd {
    fn drop(&mut self) {
        match self {
            Fd::Pipe { pipe, end } => pipe.release(*end),
            Fd::Socket { pair, side } => pair.close(*side),
            _ => {}
        }
    }
}

/// Cheap classification of a descriptor for syscall dispatch.
#[derive(Clone, Copy, PartialEq)]
pub enum FdKind {
    Closed,
    Terminal,
    File,
    /// A regular file on the persistent mount.
    Vfs,
    /// A pipe end (either direction).
    Pipe,
    /// A socket-pair side.
    Socket,
    /// An eventfd counter.
    EventFd,
    /// An epoll instance.
    Epoll,
    /// A bound `AF_UNIX` listener.
    Listener,
    /// A socket not yet bound or connected.
    Unbound,
    /// An `AF_INET` socket in any state.
    Inet,
}

pub(super) fn new_fds() -> [Fd; FD_COUNT] {
    // 0/1/2 are the standard streams.
    core::array::from_fn(|i| if i < 3 { Fd::Terminal } else { Fd::Closed })
}

/// Copy a descriptor table (for `fork`): every entry shares its open file
/// description with the parent's, and pipe references are retained.
pub(super) fn clone_fds(fds: &[Fd; FD_COUNT]) -> [Fd; FD_COUNT] {
    core::array::from_fn(|i| fds[i].clone())
}

/// The descriptor table an `execve`d program starts with: a copy of the
/// caller's, except that entries marked `FD_CLOEXEC` are left closed.
pub(super) fn clone_fds_exec(fds: &[Fd; FD_COUNT], flags: &[u16; FD_COUNT]) -> [Fd; FD_COUNT] {
    core::array::from_fn(|i| {
        if flags[i] & FD_CLOEXEC != 0 {
            Fd::Closed
        } else {
            fds[i].clone()
        }
    })
}
