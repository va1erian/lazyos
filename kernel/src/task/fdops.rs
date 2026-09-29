//! File-descriptor table operations: open, replace, close, dup and status flags.

use super::*;

/// Allocate the lowest free descriptor (>= 3) for `entry`.
pub fn fd_open(entry: Fd) -> Option<usize> {
    let mut tasks = TASKS.lock();
    let task = tasks[current()].as_mut()?;
    for index in 3..FD_COUNT {
        if matches!(task.fds[index], Fd::Closed) {
            task.fds[index] = entry;
            task.fd_flags[index] = 0;
            return Some(index);
        }
    }
    None
}

/// Replace an open descriptor's entry, returning false for a closed slot. The
/// old entry is returned for unlocked dropping by the caller (`bind` upgrades
/// an unbound socket, `connect` a connected one).
pub fn fd_replace(fd: usize, entry: Fd) -> Result<Fd, ()> {
    let mut tasks = TASKS.lock();
    let Some(task) = tasks[current()].as_mut() else {
        return Err(());
    };
    if fd >= FD_COUNT || matches!(task.fds[fd], Fd::Closed) {
        return Err(());
    }
    Ok(core::mem::replace(&mut task.fds[fd], entry))
}

/// Clone an open descriptor's entry, or `None` for a closed slot.
pub fn fd_clone(fd: usize) -> Option<Fd> {
    let tasks = TASKS.lock();
    let task = tasks[current()].as_ref()?;
    if fd >= FD_COUNT {
        return None;
    }
    match task.fds[fd] {
        Fd::Closed => None,
        _ => Some(task.fds[fd].clone()),
    }
}

/// Close a descriptor. The old entry is dropped after the task table is
/// unlocked: dropping a pipe end wakes its peer, and wait-queue notification
/// takes the task table (queue-before-table lock order). Any epoll instance in
/// this task that registered the descriptor drops the interest too, so a
/// reused descriptor number cannot inherit a stale registration.
pub fn fd_close(fd: usize) -> bool {
    let (old, epolls) = {
        let mut tasks = TASKS.lock();
        match tasks[current()].as_mut() {
            Some(task) if fd < FD_COUNT && !matches!(task.fds[fd], Fd::Closed) => {
                task.fd_flags[fd] = 0;
                let epolls: Vec<Arc<Epoll>> = task
                    .fds
                    .iter()
                    .filter_map(|entry| match entry {
                        Fd::Epoll { epoll } => Some(Arc::clone(epoll)),
                        _ => None,
                    })
                    .collect();
                (
                    Some(core::mem::replace(&mut task.fds[fd], Fd::Closed)),
                    epolls,
                )
            }
            _ => (None, Vec::new()),
        }
    };
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
    match tasks[current()].as_ref() {
        Some(task) if fd < FD_COUNT => match task.fds[fd] {
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
        },
        _ => FdKind::Closed,
    }
}

/// Whether `fd` has `FD_CLOEXEC` set (false for a closed slot).
pub fn fd_cloexec(fd: usize) -> bool {
    let tasks = TASKS.lock();
    match tasks[current()].as_ref() {
        Some(task) if fd < FD_COUNT && !matches!(task.fds[fd], Fd::Closed) => {
            task.fd_flags[fd] & FD_CLOEXEC != 0
        }
        _ => false,
    }
}

/// Set or clear `FD_CLOEXEC` on `fd`; `false` for a closed slot.
pub fn fd_set_cloexec(fd: usize, on: bool) -> bool {
    let mut tasks = TASKS.lock();
    match tasks[current()].as_mut() {
        Some(task) if fd < FD_COUNT && !matches!(task.fds[fd], Fd::Closed) => {
            if on {
                task.fd_flags[fd] |= FD_CLOEXEC;
            } else {
                task.fd_flags[fd] &= !FD_CLOEXEC;
            }
            true
        }
        _ => false,
    }
}

/// Close every descriptor marked `FD_CLOEXEC` (the `execve` step). Returns how
/// many were closed.
pub fn fd_close_cloexec() -> usize {
    let mut closed = 0;
    for fd in 0..FD_COUNT {
        if fd_cloexec(fd) && fd_close(fd) {
            closed += 1;
        }
    }
    closed
}

/// Linux `O_NONBLOCK` (as `fd_status`/`fd_set_status` carry it).
pub const O_NONBLOCK: u64 = 0o4000;

/// The access-mode bits of `F_GETFL` for `fd`, or `None` for a closed slot.
/// `O_NONBLOCK` is reported from the pipe/socket's open file description, so a
/// `dup` or `fork` sees the same setting.
pub fn fd_status(fd: usize) -> Option<u64> {
    let tasks = TASKS.lock();
    let task = tasks[current()].as_ref()?;
    if fd >= FD_COUNT {
        return None;
    }
    match &task.fds[fd] {
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
    if fd >= FD_COUNT {
        return false;
    }
    match &mut task.fds[fd] {
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
            *flag = nonblock;
            true
        }
    }
}
