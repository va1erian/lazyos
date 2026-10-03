//! File-descriptor table operations: open, replace, close, dup and status flags.

use super::*;

/// Allocate the lowest free descriptor (>= 3) for `entry`. `None` when the
/// table is at `limit.fd_max` (the entry is dropped after the table unlocks).
pub fn fd_open(entry: Fd) -> Option<usize> {
    let mut junk = Vec::new();
    let opened = {
        let mut tasks = TASKS.lock();
        let me = current();
        let task = tasks[me].as_mut()?;
        match task.fds.install_lowest(3, entry) {
            Ok(fd) => {
                fdshare::mirror_fd(&mut tasks, me, fd, &mut junk);
                Some(fd)
            }
            Err(entry) => {
                junk.push(entry);
                None
            }
        }
    };
    drop(junk);
    opened
}

/// Replace an open descriptor's entry, returning false for a closed slot. The
/// old entry is returned for unlocked dropping by the caller (`bind` upgrades
/// an unbound socket, `connect` a connected one).
pub fn fd_replace(fd: usize, entry: Fd) -> Result<Fd, ()> {
    let refused = {
        let mut tasks = TASKS.lock();
        let Some(task) = tasks[current()].as_mut() else {
            return Err(());
        };
        match task.fds.replace(fd, entry) {
            Ok(old) => {
                let mut junk = Vec::new();
                fdshare::mirror_fd(&mut tasks, current(), fd, &mut junk);
                drop(tasks);
                drop(junk);
                return Ok(old);
            }
            Err(entry) => entry,
        }
    };
    drop(refused);
    Err(())
}

/// Clone an open descriptor's entry, or `None` for a closed slot.
pub fn fd_clone(fd: usize) -> Option<Fd> {
    let tasks = TASKS.lock();
    tasks[current()].as_ref()?.fds.get(fd).cloned()
}

/// Close a descriptor. The old entry is dropped after the task table is
/// unlocked: dropping a pipe end wakes its peer, and wait-queue notification
/// takes the task table (queue-before-table lock order). Any epoll instance in
/// this task that registered the descriptor drops the interest too, so a
/// reused descriptor number cannot inherit a stale registration.
pub fn fd_close(fd: usize) -> bool {
    let mut junk = Vec::new();
    let (old, epolls) = {
        let mut tasks = TASKS.lock();
        let me = current();
        let closed = match tasks[me].as_mut() {
            Some(task) if task.fds.is_open(fd) => {
                let epolls: Vec<Arc<Epoll>> = task
                    .fds
                    .iter()
                    .filter_map(|(_, entry)| match entry {
                        Fd::Epoll { epoll } => Some(Arc::clone(epoll)),
                        _ => None,
                    })
                    .collect();
                (task.fds.take(fd), epolls)
            }
            _ => (None, Vec::new()),
        };
        if closed.0.is_some() {
            fdshare::mirror_fd(&mut tasks, me, fd, &mut junk);
        }
        closed
    };
    drop(junk);
    for epoll in &epolls {
        Epoll::drop_fd(epoll, fd);
    }
    let closed = old.is_some();
    drop(old);
    closed
}

/// Classify a descriptor.
pub fn fd_kind(fd: usize) -> FdKind {
    let tasks = TASKS.lock();
    match tasks[current()].as_ref().and_then(|task| task.fds.get(fd)) {
        Some(entry) => match entry {
            Fd::Closed => FdKind::Closed,
            Fd::Terminal => FdKind::Terminal,
            Fd::File { .. } => FdKind::File,
            Fd::Vfs { .. } => FdKind::Vfs,
            Fd::Pipe { .. } => FdKind::Pipe,
            Fd::Socket { .. } => FdKind::Socket,
            Fd::Event { .. } => FdKind::EventFd,
            Fd::Epoll { .. } => FdKind::Epoll,
            Fd::UnixListener { .. } => FdKind::Listener,
            Fd::Unbound { .. } => FdKind::Unbound,
            Fd::Inet { .. } => FdKind::Inet,
            Fd::Pty { .. } => FdKind::Pty,
        },
        _ => FdKind::Closed,
    }
}

/// Whether `fd` has `FD_CLOEXEC` set (false for a closed slot).
pub fn fd_cloexec(fd: usize) -> bool {
    let tasks = TASKS.lock();
    tasks[current()]
        .as_ref()
        .and_then(|task| task.fds.flags(fd))
        .is_some_and(|flags| flags & FD_CLOEXEC != 0)
}

/// Set or clear `FD_CLOEXEC` on `fd`; `false` for a closed slot.
pub fn fd_set_cloexec(fd: usize, on: bool) -> bool {
    let mut junk = Vec::new();
    let mut tasks = TASKS.lock();
    let me = current();
    let Some(task) = tasks[me].as_mut() else {
        return false;
    };
    let Some(flags) = task.fds.flags(fd) else {
        return false;
    };
    let flags = if on {
        flags | FD_CLOEXEC
    } else {
        flags & !FD_CLOEXEC
    };
    let set = task.fds.set_flags(fd, flags);
    fdshare::mirror_fd(&mut tasks, me, fd, &mut junk);
    drop(tasks);
    drop(junk);
    set
}

/// Close every descriptor marked `FD_CLOEXEC` (the `execve` step). Returns how
/// many were closed.
pub fn fd_close_cloexec() -> usize {
    let marked = {
        let tasks = TASKS.lock();
        match tasks[current()].as_ref() {
            Some(task) => task.fds.cloexec_fds(),
            None => return 0,
        }
    };
    marked.into_iter().filter(|&fd| fd_close(fd)).count()
}

/// Linux `O_NONBLOCK` (as `fd_status`/`fd_set_status` carry it).
pub const O_NONBLOCK: u64 = 0o4000;

/// The access-mode bits of `F_GETFL` for `fd`, or `None` for a closed slot.
/// `O_NONBLOCK` is reported from the pipe/socket's open file description, so a
/// `dup` or `fork` sees the same setting.
pub fn fd_status(fd: usize) -> Option<u64> {
    let tasks = TASKS.lock();
    let task = tasks[current()].as_ref()?;
    match task.fds.get(fd)? {
        Fd::Closed => None,
        Fd::Terminal | Fd::File { .. } => Some(0), // O_RDONLY
        Fd::Vfs { file } => Some(file.status_flags()),
        Fd::Pipe { pipe, end } => {
            let access = match end {
                End::Read => 0,
                End::Write => 1, // O_WRONLY
            };
            Some(access | (u64::from(pipe.nonblock(*end)) * O_NONBLOCK))
        }
        Fd::Socket { pair, side } => {
            Some(2 | (u64::from(pair.nonblock(*side)) * O_NONBLOCK)) // O_RDWR
        }
        Fd::Event { event } => Some(2 | (u64::from(event.nonblock()) * O_NONBLOCK)),
        Fd::Epoll { epoll } => Some(2 | (u64::from(epoll.nonblock()) * O_NONBLOCK)),
        Fd::UnixListener { listener } => Some(2 | (u64::from(listener.nonblock()) * O_NONBLOCK)),
        Fd::Unbound { nonblock: flag, .. } => Some(2 | (u64::from(*flag) * O_NONBLOCK)),
        Fd::Inet { sock } => Some(2 | (u64::from(sock.nonblock()) * O_NONBLOCK)),
        Fd::Pty { pty, master } => Some(2 | (u64::from(pty.nonblock(*master)) * O_NONBLOCK)),
    }
}

/// Apply `F_SETFL`: only `O_NONBLOCK` is meaningful (pipes, sockets, eventfds,
/// epolls and listeners); other status flags are accepted and ignored. `false`
/// for a closed slot.
pub fn fd_set_status(fd: usize, nonblock: bool) -> bool {
    let mut tasks = TASKS.lock();
    let Some(task) = tasks[current()].as_mut() else {
        return false;
    };
    let Some(entry) = task.fds.get_mut(fd) else {
        return false;
    };
    match entry {
        Fd::Closed => false,
        Fd::Terminal | Fd::File { .. } | Fd::Vfs { .. } => true,
        Fd::Pipe { pipe, end } => {
            pipe.set_nonblock(*end, nonblock);
            true
        }
        Fd::Socket { pair, side } => {
            pair.set_nonblock(*side, nonblock);
            true
        }
        Fd::Event { event } => {
            event.set_nonblock(nonblock);
            true
        }
        Fd::Epoll { epoll } => {
            epoll.set_nonblock(nonblock);
            true
        }
        Fd::UnixListener { listener } => {
            listener.set_nonblock(nonblock);
            true
        }
        Fd::Unbound { nonblock: flag, .. } => {
            // The flag lives in the entry itself, so a shared table needs it
            // copied (the other kinds keep it on the shared object).
            *flag = nonblock;
            let mut junk = Vec::new();
            fdshare::mirror_fd(&mut tasks, current(), fd, &mut junk);
            drop(tasks);
            drop(junk);
            true
        }
        Fd::Inet { sock } => {
            sock.set_nonblock(nonblock);
            true
        }
        Fd::Pty { pty, master } => {
            pty.set_nonblock(*master, nonblock);
            true
        }
    }
}
