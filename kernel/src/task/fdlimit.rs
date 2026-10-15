//! `RLIMIT_NOFILE` for a process: the soft and hard descriptor limits a
//! program reads with `getrlimit` and changes with `setrlimit`.
//!
//! The pair lives in each thread's [`FdTable`](super::FdTable) (a table the
//! thread shares with its group is mirrored, see [`super::fdshare`]), so the
//! code that grows a table reads it without a lookup. `setrlimit` therefore
//! writes it into every task of the caller's thread group. A new limit is
//! `soft <= hard <= limit.fd_max`, and the hard limit only ever goes down
//! (raising it needs a privilege no LazyOS program has); descriptors above a
//! lowered soft limit stay open, as on Linux, but no new one is made past it.

use super::*;

/// Why a `(soft, hard)` request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitError {
    /// `soft` is above `hard`.
    Inverted,
    /// `hard` is above the current hard limit, or the caller is not the owner.
    NotPermitted,
}

/// The `(soft, hard)` descriptor limits of process `pid`, or `None` when no
/// such task exists.
pub fn nofile_limits_of(pid: usize) -> Option<(usize, usize)> {
    let tasks = TASKS.lock();
    let task = tasks.get(pid)?.as_ref()?;
    Some((task.fds.soft_limit(), task.fds.hard_limit()))
}

/// The calling task's `(soft, hard)` descriptor limits.
pub fn nofile_limits() -> (usize, usize) {
    nofile_limits_of(current()).unwrap_or((fd_max(), fd_max()))
}

/// The most descriptors the calling task's table may hold.
pub fn fd_soft_limit() -> usize {
    nofile_limits().0
}

/// Set the calling process's limits, in every thread of its group.
pub fn set_nofile_limits(soft: usize, hard: usize) -> Result<(), LimitError> {
    if soft > hard {
        return Err(LimitError::Inverted);
    }
    let mut tasks = TASKS.lock();
    let me = current();
    let leader = family::leader_of(&tasks, me);
    let current_hard = tasks[me]
        .as_ref()
        .ok_or(LimitError::NotPermitted)?
        .fds
        .hard_limit();
    if hard > current_hard {
        return Err(LimitError::NotPermitted);
    }
    let members: Vec<usize> = (0..MAX_TASKS)
        .filter(|&slot| {
            tasks[slot]
                .as_ref()
                .is_some_and(|task| task.state != TaskState::Done)
                && family::leader_of(&tasks, slot) == leader
        })
        .collect();
    for slot in members {
        if let Some(task) = tasks[slot].as_mut() {
            task.fds.set_limits(soft, hard);
        }
    }
    Ok(())
}
