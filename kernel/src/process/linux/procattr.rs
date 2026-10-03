//! Per-process attributes and hints: `prctl`, `set_robust_list`/
//! `get_robust_list` (and the robust-futex walk at thread exit), the
//! supplementary group list (`getgroups`/`setgroups`), `sched_yield`, the
//! scheduler queries, and `madvise`.
//!
//! Each call either does what Linux does or refuses what it cannot honour
//! (`EINVAL`/`EPERM`); none answers success for an effect that does not happen.

use crate::ipc::credentials::{self, CAP_SETUID};
use crate::task;
use crate::user_ptr;

use super::errno::{err, EFAULT, EINVAL, EPERM};
use super::futex::futex_wake;

const PR_SET_PDEATHSIG: u64 = 1;
const PR_GET_PDEATHSIG: u64 = 2;
const PR_GET_DUMPABLE: u64 = 3;
const PR_SET_DUMPABLE: u64 = 4;
const PR_SET_NAME: u64 = 15;
const PR_GET_NAME: u64 = 16;
const PR_GET_TIMERSLACK: u64 = 30;
const PR_SET_TIMERSLACK: u64 = 29;
const PR_SET_NO_NEW_PRIVS: u64 = 38;
const PR_GET_NO_NEW_PRIVS: u64 = 39;

/// `prctl(option, arg2, ...)`.
///
/// * `PR_SET_NAME`/`PR_GET_NAME`: the 16-byte thread name;
/// * `PR_SET_DUMPABLE`/`PR_GET_DUMPABLE`: no core is ever written, so `0`
///   and `1` are both honoured and `GET` reports `1`;
/// * `PR_SET_NO_NEW_PRIVS`/`GET`: recorded and inherited (LazyOS `execve`
///   never grants privileges, so the promise holds trivially);
/// * `PR_SET_PDEATHSIG` with signal 0 (no signal: the default), `GET` = 0;
/// * `PR_SET_TIMERSLACK`/`GET`: the timer is 100 Hz, slack is moot.
///
/// Everything else, including a parent-death signal that would never be sent,
/// is `EINVAL`.
pub(super) fn sys_prctl(option: u64, arg2: u64, arg3: u64) -> u64 {
    match option {
        PR_SET_NAME => {
            let mut name = [0u8; 16];
            for (index, byte) in name.iter_mut().take(15).enumerate() {
                match user_ptr::try_read::<u8>(arg2 + index as u64) {
                    Ok(0) => break,
                    Ok(value) => *byte = value,
                    Err(_) => return err(EFAULT),
                }
            }
            task::linuxstate::with_extras(|extras| extras.comm = name);
            0
        }
        PR_GET_NAME => {
            let (name, _) = task::linuxstate::comm(task::current());
            match user_ptr::try_copy_to(arg2, &name) {
                Ok(()) => 0,
                Err(_) => err(EFAULT),
            }
        }
        PR_GET_DUMPABLE => 1,
        PR_SET_DUMPABLE if arg2 <= 1 => 0,
        PR_SET_NO_NEW_PRIVS if arg2 == 1 && arg3 == 0 => {
            task::linuxstate::with_extras(|extras| extras.no_new_privs = true);
            0
        }
        PR_GET_NO_NEW_PRIVS => {
            u64::from(task::linuxstate::with_extras(|extras| extras.no_new_privs).unwrap_or(false))
        }
        PR_SET_PDEATHSIG if arg2 == 0 => 0,
        PR_GET_PDEATHSIG => match user_ptr::try_write::<i32>(arg2, 0) {
            Ok(()) => 0,
            Err(_) => err(EFAULT),
        },
        PR_SET_TIMERSLACK => 0,
        PR_GET_TIMERSLACK => 10_000_000,
        _ => err(EINVAL),
    }
}

/// `struct robust_list_head` is three words.
const ROBUST_HEAD_LEN: u64 = 24;
/// Robust futex word bits.
const FUTEX_WAITERS: u32 = 0x8000_0000;
const FUTEX_OWNER_DIED: u32 = 0x4000_0000;
const FUTEX_TID_MASK: u32 = 0x3fff_ffff;
/// Most list entries one exit walks (the kernel's `ROBUST_LIST_LIMIT`), so a
/// corrupt or cyclic list cannot hold the exit up.
const ROBUST_LIST_LIMIT: usize = 2048;

/// `set_robust_list(head, len)`.
pub(super) fn sys_set_robust_list(head: u64, len: u64) -> u64 {
    if len != ROBUST_HEAD_LEN {
        return err(EINVAL);
    }
    task::linuxstate::with_extras(|extras| extras.robust_head = head);
    0
}

/// `get_robust_list(pid, head_ptr, len_ptr)`: the caller's own (pid 0 or its
/// tid); another task's list is not ours to read (`EPERM`).
pub(super) fn sys_get_robust_list(pid: u64, head_ptr: u64, len_ptr: u64) -> u64 {
    if pid != 0 && pid as usize != task::current() {
        return err(EPERM);
    }
    let head = task::linuxstate::with_extras(|extras| extras.robust_head).unwrap_or(0);
    if user_ptr::try_write::<u64>(head_ptr, head).is_err()
        || user_ptr::try_write::<u64>(len_ptr, ROBUST_HEAD_LEN).is_err()
    {
        return err(EFAULT);
    }
    0
}

/// Release the robust futexes the exiting thread `tid` still holds: each gets
/// `FUTEX_OWNER_DIED` and one waiter is woken, so a robust mutex's next locker
/// sees `EOWNERDEAD` instead of hanging. Runs in the exiting thread's address
/// space, before its slot is finished. Bad pointers end the walk quietly, as
/// on Linux (the thread is going away; there is nobody to report to).
pub(super) fn exit_robust_list(tid: usize) {
    let head = task::linuxstate::with_extras(|extras| core::mem::take(&mut extras.robust_head))
        .unwrap_or(0);
    if head == 0 {
        return;
    }
    let read = |addr: u64| user_ptr::try_read::<u64>(addr).ok();
    let (Some(first), Some(offset), Some(pending)) = (read(head), read(head + 8), read(head + 16))
    else {
        return;
    };
    let offset = offset as i64;
    let mut entry = first;
    for _ in 0..ROBUST_LIST_LIMIT {
        if entry == head || entry == 0 {
            break;
        }
        let next = read(entry);
        // The entry being locked or unlocked right now is handled below.
        if entry != pending {
            release_robust(entry, offset, tid);
        }
        match next {
            Some(next) => entry = next,
            None => break,
        }
    }
    if pending != 0 {
        release_robust(pending, offset, tid);
    }
}

/// Mark one robust futex (at `entry + offset`) owner-dead if `tid` owns it.
fn release_robust(entry: u64, offset: i64, tid: usize) {
    let word = entry.wrapping_add(offset as u64);
    if !word.is_multiple_of(4) {
        return;
    }
    let Ok(value) = user_ptr::try_read::<u32>(word) else {
        return;
    };
    if value & FUTEX_TID_MASK != tid as u32 {
        return;
    }
    let released = (value & FUTEX_WAITERS) | FUTEX_OWNER_DIED;
    if user_ptr::try_write::<u32>(word, released).is_ok() && value & FUTEX_WAITERS != 0 {
        futex_wake(word, 1);
    }
}

/// `getgroups(size, list)`: LazyOS has no supplementary groups, so the list
/// is empty.
pub(super) fn sys_getgroups(size: u64, _list: u64) -> u64 {
    if (size as i32) < 0 {
        return err(EINVAL);
    }
    0
}

/// `setgroups(size, list)`: privileged; only a list LazyOS can represent
/// (empty, or nothing but the caller's own gid) is accepted.
pub(super) fn sys_setgroups(size: u64, list: u64) -> u64 {
    let cred = credentials::of(task::current());
    if !cred.has_cap(CAP_SETUID) {
        return err(EPERM);
    }
    if size > 65536 {
        return err(EINVAL);
    }
    for index in 0..size {
        match user_ptr::try_read::<u32>(list + index * 4) {
            Ok(gid) if gid == cred.gid => {}
            Ok(_) => return err(EINVAL),
            Err(_) => return err(EFAULT),
        }
    }
    0
}

/// `sched_yield()`: give the CPU to another runnable task, if any.
pub(super) fn sys_sched_yield() -> u64 {
    task::switch::yield_now();
    0
}

/// `sched_getscheduler(pid)`: every task is `SCHED_OTHER`.
pub(super) fn sys_sched_getscheduler(pid: u64) -> u64 {
    if (pid as i64) < 0 {
        return err(EINVAL);
    }
    0
}

/// `sched_getparam(pid, param)`: priority 0, as for any `SCHED_OTHER` task.
pub(super) fn sys_sched_getparam(pid: u64, param: u64) -> u64 {
    if (pid as i64) < 0 {
        return err(EINVAL);
    }
    match user_ptr::try_write::<i32>(param, 0) {
        Ok(()) => 0,
        Err(_) => err(EFAULT),
    }
}

/// `sched_get_priority_max/min(policy)`: `SCHED_OTHER`/`BATCH`/`IDLE` have
/// only priority 0; the real-time policies are not offered.
pub(super) fn sys_sched_priority_bound(policy: u64) -> u64 {
    match policy {
        0 | 3 | 5 => 0,
        _ => err(EINVAL),
    }
}

const MADV_NORMAL: u64 = 0;
const MADV_RANDOM: u64 = 1;
const MADV_SEQUENTIAL: u64 = 2;
const MADV_WILLNEED: u64 = 3;
const MADV_DONTNEED: u64 = 4;
const MADV_FREE: u64 = 8;

/// `madvise(addr, len, advice)`.
///
/// The access-pattern hints and the "keep or drop from dumps/huge pages/KSM"
/// family change nothing observable, so they succeed. `MADV_FREE` lets the
/// kernel discard the pages lazily; keeping them is a valid outcome. Only
/// `MADV_DONTNEED` has a contract a program can see (anonymous memory reads
/// back as zeros), and it is implemented by [`super::mem::dontneed`]. Any
/// other advice (`MADV_REMOVE`, `MADV_HWPOISON`, ...) is `EINVAL`.
pub(super) fn sys_madvise(addr: u64, len: u64, advice: u64) -> u64 {
    if !addr.is_multiple_of(4096) {
        return err(EINVAL);
    }
    if addr.checked_add(len).is_none() {
        return err(EINVAL);
    }
    match advice {
        MADV_DONTNEED => super::mem::dontneed(addr, len),
        MADV_NORMAL | MADV_RANDOM | MADV_SEQUENTIAL | MADV_WILLNEED | MADV_FREE => 0,
        // MERGEABLE/UNMERGEABLE, HUGEPAGE/NOHUGEPAGE, DONTDUMP/DODUMP, COLD,
        // PAGEOUT, POPULATE_READ/WRITE are pure hints here. WIPEONFORK (18)
        // is refused: `fork` copies every page.
        12..=17 | 19..=23 => 0,
        _ => err(EINVAL),
    }
}
