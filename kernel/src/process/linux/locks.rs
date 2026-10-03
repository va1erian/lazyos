//! Advisory file locks: `flock(2)` and the `fcntl` record locks (`F_GETLK`,
//! `F_SETLK`, `F_SETLKW`, and the open-file-description `F_OFD_*` forms).
//!
//! One table holds every lock: the file (by its path, the identity every
//! descriptor kind records), the owner, the byte range and read/write. The two
//! families never conflict with each other, as on Linux. Owners:
//!
//! * a POSIX record lock belongs to the *process* (thread group), so a second
//!   lock from the same process replaces the overlapping part of its own;
//! * a `flock` lock and an OFD lock belong to the *open file description*, so
//!   `dup`/`fork` copies share it and it ends when the last copy closes.
//!
//! Locks are released explicitly, or lazily when their owner is gone: a
//! process that exited, or a description no descriptor refers to any more.
//! Every lock operation sweeps those first, and a `F_SETLKW`/blocking `flock`
//! waiter re-checks every 100 ms, so it never waits on a dead owner for long.
//! One deviation from POSIX: closing *one* descriptor of a file does not drop
//! the process's record locks on it (they go when the process exits or
//! unlocks); a program that relies on that, rather than suffering from it, is
//! rare. There is no deadlock detection (`EDEADLK` is never returned).

use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use spin::Mutex;

use crate::fs::openfile::OpenFile;
use crate::task::wait::WaitQueue;
use crate::task::{self, Fd, SnapFile, WaitKind, WakeReason};
use crate::user_ptr;

use super::errno::{err, EAGAIN, EBADF, EFAULT, EINTR, EINVAL};

/// Lock waiters; every unlock (and the periodic re-check) wakes them all.
static WAITERS: WaitQueue = WaitQueue::new(WaitKind::Poll);
static LOCKS: Mutex<Vec<Lock>> = Mutex::new(Vec::new());

/// Re-check interval of a blocked lock request, in 100 Hz ticks.
const RECHECK_TICKS: u64 = 10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Family {
    Posix,
    Flock,
}

/// Who holds a lock (see the module docs).
#[derive(Clone)]
enum Owner {
    /// A thread group: its leader's slot and address space.
    Process {
        slot: usize,
        space: u64,
    },
    /// An open file description.
    Snap(Weak<SnapFile>),
    Vfs(Weak<OpenFile>),
}

impl Owner {
    fn same(&self, other: &Owner) -> bool {
        match (self, other) {
            (Owner::Process { slot: a, space: x }, Owner::Process { slot: b, space: y }) => {
                a == b && x == y
            }
            (Owner::Snap(a), Owner::Snap(b)) => Weak::ptr_eq(a, b),
            (Owner::Vfs(a), Owner::Vfs(b)) => Weak::ptr_eq(a, b),
            _ => false,
        }
    }

    fn alive(&self) -> bool {
        match self {
            Owner::Process { slot, space } => task::linuxstate::running_in(*slot, *space),
            Owner::Snap(weak) => weak.strong_count() > 0,
            Owner::Vfs(weak) => weak.strong_count() > 0,
        }
    }

    /// The pid `F_GETLK` reports (-1 for a description, as Linux does for
    /// OFD locks).
    fn pid(&self) -> i32 {
        match self {
            Owner::Process { slot, .. } => *slot as i32,
            _ => -1,
        }
    }
}

#[derive(Clone)]
struct Lock {
    file: String,
    family: Family,
    owner: Owner,
    start: u64,
    /// Exclusive end; `u64::MAX` reaches past any end of file.
    end: u64,
    write: bool,
}

impl Lock {
    fn overlaps(&self, start: u64, end: u64) -> bool {
        self.start < end && start < self.end
    }
}

/// One lock request.
struct Request {
    file: String,
    family: Family,
    owner: Owner,
    start: u64,
    end: u64,
    /// `None` unlocks.
    write: Option<bool>,
}

/// The first lock that keeps `request` from being granted.
fn conflict(locks: &[Lock], request: &Request) -> Option<Lock> {
    let write = request.write?;
    locks
        .iter()
        .find(|lock| {
            lock.family == request.family
                && lock.file == request.file
                && !lock.owner.same(&request.owner)
                && lock.overlaps(request.start, request.end)
                && (lock.write || write)
        })
        .cloned()
}

/// Remove `owner`'s part of `[start, end)` on the file, splitting a lock
/// that straddles the range.
fn carve(locks: &mut Vec<Lock>, request: &Request) {
    let mut kept = Vec::with_capacity(locks.len() + 1);
    for lock in locks.drain(..) {
        let mine = lock.family == request.family
            && lock.file == request.file
            && lock.owner.same(&request.owner);
        if !mine || !lock.overlaps(request.start, request.end) {
            kept.push(lock);
            continue;
        }
        if lock.start < request.start {
            let mut left = lock.clone();
            left.end = request.start;
            kept.push(left);
        }
        if lock.end > request.end {
            let mut right = lock;
            right.start = request.end;
            kept.push(right);
        }
    }
    *locks = kept;
}

/// Try to apply `request`; `Err(conflicting lock)` when it must wait.
fn try_apply(request: &Request) -> Result<(), Lock> {
    let mut locks = LOCKS.lock();
    locks.retain(|lock| lock.owner.alive());
    if let Some(blocker) = conflict(&locks, request) {
        return Err(blocker);
    }
    carve(&mut locks, request);
    if let Some(write) = request.write {
        locks.push(Lock {
            file: request.file.clone(),
            family: request.family,
            owner: request.owner.clone(),
            start: request.start,
            end: request.end,
            write,
        });
    }
    Ok(())
}

/// Apply `request`, waiting for conflicting holders when `wait`.
fn apply(request: Request, wait: bool) -> u64 {
    loop {
        match try_apply(&request) {
            Ok(()) => {
                WAITERS.notify_all();
                return 0;
            }
            Err(_) if !wait => return err(EAGAIN),
            Err(_) => {
                let deadline = task::ticks() + RECHECK_TICKS;
                if WAITERS.wait(task::current(), Some(deadline)) == WakeReason::Interrupted {
                    return err(EINTR);
                }
            }
        }
    }
}

/// The file and the description behind `fd`, or `EBADF`/`EINVAL`.
fn target(fd: u64) -> Result<(String, Owner), u64> {
    match task::fd_clone(fd as usize) {
        Some(Fd::File { ref file }) => match task::fd_file_meta(fd as usize).and_then(|m| m.path) {
            Some(path) => Ok((path, Owner::Snap(Arc::downgrade(file)))),
            None => Err(err(EINVAL)),
        },
        Some(Fd::Vfs { ref file }) => Ok((file.path(), Owner::Vfs(Arc::downgrade(file)))),
        Some(Fd::Closed) | None => Err(err(EBADF)),
        Some(_) => Err(err(EINVAL)),
    }
}

/// The calling process as a lock owner.
fn this_process() -> Owner {
    let slot = task::linuxstate::tgid();
    Owner::Process {
        slot,
        space: task::pml4_of(slot).unwrap_or(0),
    }
}

/// `flock(fd, operation)`: `LOCK_SH`, `LOCK_EX`, `LOCK_UN`, optionally with
/// `LOCK_NB`. A whole-file lock owned by the open file description.
pub(super) fn sys_flock(fd: u64, operation: u64) -> u64 {
    const LOCK_SH: u64 = 1;
    const LOCK_EX: u64 = 2;
    const LOCK_NB: u64 = 4;
    const LOCK_UN: u64 = 8;
    let (file, owner) = match target(fd) {
        Ok(target) => target,
        Err(code) => return code,
    };
    let write = match operation & !LOCK_NB {
        LOCK_SH => Some(false),
        LOCK_EX => Some(true),
        LOCK_UN => None,
        _ => return err(EINVAL),
    };
    let request = Request {
        file,
        family: Family::Flock,
        owner,
        start: 0,
        end: u64::MAX,
        write,
    };
    apply(request, operation & LOCK_NB == 0)
}

const F_RDLCK: i16 = 0;
const F_WRLCK: i16 = 1;
const F_UNLCK: i16 = 2;

/// A decoded `struct flock`: `(type, start, end)`.
fn read_flock(fd: u64, ptr: u64) -> Result<(i16, u64, u64), u64> {
    let fault = |_| err(EFAULT);
    let kind = user_ptr::try_read::<u16>(ptr).map_err(fault)? as i16;
    let whence = user_ptr::try_read::<u16>(ptr + 2).map_err(fault)? as i16;
    let start = user_ptr::try_read::<i64>(ptr + 8).map_err(fault)?;
    let len = user_ptr::try_read::<i64>(ptr + 16).map_err(fault)?;
    let base = match whence {
        0 => 0,
        1 => task::fd_offset(fd as usize).unwrap_or(0) as i64,
        2 => task::fd_size(fd as usize).unwrap_or(0) as i64,
        _ => return Err(err(EINVAL)),
    };
    let start = base.checked_add(start).ok_or(err(EINVAL))?;
    let (from, to) = match len {
        0 => (start, i64::MAX),
        len if len > 0 => (start, start.saturating_add(len)),
        len => (start + len, start),
    };
    if from < 0 {
        return Err(err(EINVAL));
    }
    let end = if to == i64::MAX { u64::MAX } else { to as u64 };
    Ok((kind, from as u64, end))
}

/// Write the `F_GETLK` answer: the blocking lock, or `F_UNLCK`.
fn write_flock(ptr: u64, blocker: Option<&Lock>) -> Result<(), u64> {
    let mut out = [0u8; 32];
    match blocker {
        None => out[0..2].copy_from_slice(&F_UNLCK.to_le_bytes()),
        Some(lock) => {
            let kind = if lock.write { F_WRLCK } else { F_RDLCK };
            let len = if lock.end == u64::MAX {
                0
            } else {
                (lock.end - lock.start) as i64
            };
            out[0..2].copy_from_slice(&kind.to_le_bytes());
            out[8..16].copy_from_slice(&(lock.start as i64).to_le_bytes());
            out[16..24].copy_from_slice(&len.to_le_bytes());
            out[24..28].copy_from_slice(&lock.owner.pid().to_le_bytes());
        }
    }
    user_ptr::try_copy_to(ptr, &out).map_err(|_| err(EFAULT))
}

/// The `fcntl` lock commands (`F_GETLK` 5, `F_SETLK` 6, `F_SETLKW` 7, and the
/// OFD forms 36-38); `None` for any other command.
pub(super) fn fcntl_lock(fd: u64, cmd: u64, arg: u64) -> Option<u64> {
    let (ofd, query, wait) = match cmd {
        5 => (false, true, false),
        6 => (false, false, false),
        7 => (false, false, true),
        36 => (true, true, false),
        37 => (true, false, false),
        38 => (true, false, true),
        _ => return None,
    };
    let (file, description) = match target(fd) {
        Ok(target) => target,
        Err(code) => return Some(code),
    };
    let (kind, start, end) = match read_flock(fd, arg) {
        Ok(decoded) => decoded,
        Err(code) => return Some(code),
    };
    let write = match kind {
        F_RDLCK => Some(false),
        F_WRLCK => Some(true),
        F_UNLCK if !query => None,
        _ => return Some(err(EINVAL)),
    };
    let owner = if ofd { description } else { this_process() };
    let request = Request {
        file,
        family: Family::Posix,
        owner,
        start,
        end,
        write,
    };
    if query {
        let blocker = {
            let mut locks = LOCKS.lock();
            locks.retain(|lock| lock.owner.alive());
            conflict(&locks, &request)
        };
        return Some(match write_flock(arg, blocker.as_ref()) {
            Ok(()) => 0,
            Err(code) => code,
        });
    }
    Some(apply(request, wait))
}

/// Locks currently held (tests).
#[cfg(lazyos_tests)]
pub fn held_for_test() -> usize {
    let mut locks = LOCKS.lock();
    locks.retain(|lock| lock.owner.alive());
    locks.len()
}
