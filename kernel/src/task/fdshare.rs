//! Keeping shared descriptor tables (and working directories) identical.
//!
//! `clone(CLONE_FILES)` threads share one descriptor table on Linux. Here each
//! task keeps its own [`FdTable`], and every operation that changes an entry calls
//! [`mirror_fd`] while it still holds the task table: the entry (and its
//! `FD_CLOEXEC` bit) is copied into every other live member of the caller's
//! share group. Each copy holds its own reference (a pipe end is acquired once
//! per copy), so a close in one thread closes it everywhere, and a thread that
//! exits drops only its own references. The replaced entries are handed back
//! to be dropped after the lock is released, for the same reason `fd_close`
//! does that (the last reference to a pipe end wakes a queue, which takes the
//! task table).
//!
//! `CLONE_FS` is the same idea for the working directory ([`mirror_cwd`]).

use super::*;

/// The other live members of `me`'s descriptor-table share group.
fn file_peers(tasks: &[Option<Task>; MAX_TASKS], me: usize) -> SlotList {
    let mut peers = SlotList::default();
    let Some(group) = tasks[me].as_ref().map(|task| task.linux.files_group) else {
        return peers;
    };
    if group == 0 {
        return peers;
    }
    for (slot, task) in tasks.iter().enumerate() {
        if slot == me {
            continue;
        }
        if let Some(task) = task {
            if task.linux.files_group == group && task.state != TaskState::Done {
                peers.push(slot);
            }
        }
    }
    peers
}

/// The slots a mirror writes to.
#[derive(Default)]
pub(super) struct SlotList {
    slots: Vec<usize>,
}

impl SlotList {
    fn push(&mut self, slot: usize) {
        self.slots.push(slot);
    }

    fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.slots.iter().copied()
    }
}

/// Copy `me`'s descriptor `fd` (entry and flags) into every peer that shares
/// its table. Replaced peer entries are pushed onto `junk`, to be dropped by
/// the caller once `TASKS` is unlocked.
pub(super) fn mirror_fd(
    tasks: &mut [Option<Task>; MAX_TASKS],
    me: usize,
    fd: usize,
    junk: &mut Vec<Fd>,
) {
    let peers = file_peers(tasks, me);
    if peers.slots.is_empty() {
        return;
    }
    let Some((entry, flags)) = tasks[me]
        .as_ref()
        .map(|task| (task.fds.get(fd).cloned(), task.fds.flags(fd).unwrap_or(0)))
    else {
        return;
    };
    for peer in peers.iter() {
        let Some(task) = tasks[peer].as_mut() else {
            continue;
        };
        match &entry {
            // `put` grows the peer's table like the caller's grew; a refusal
            // (the peer's heap) hands the copy back, which is dropped too.
            Some(entry) => {
                match task.fds.put(fd, entry.clone()) {
                    Ok(old) | Err(old) => junk.push(old),
                }
                task.fds.set_flags(fd, flags);
            }
            None => junk.extend(task.fds.take(fd)),
        }
    }
    junk.extend(entry);
}

/// Copy `me`'s working directory into every task of its `CLONE_FS` group.
/// Returns the replaced values, to be dropped after the lock.
pub(super) fn mirror_cwd(
    tasks: &mut [Option<Task>; MAX_TASKS],
    me: usize,
) -> Vec<Option<Arc<str>>> {
    let mut old = Vec::new();
    let Some((group, cwd)) = tasks[me]
        .as_ref()
        .map(|task| (task.linux.fs_group, task.cwd.clone()))
    else {
        return old;
    };
    if group == 0 {
        return old;
    }
    for (slot, task) in tasks.iter_mut().enumerate() {
        if slot == me {
            continue;
        }
        if let Some(task) = task {
            if task.linux.fs_group == group && task.state != TaskState::Done {
                old.push(core::mem::replace(&mut task.cwd, cwd.clone()));
            }
        }
    }
    old
}
