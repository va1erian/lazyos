//! Resource reporting: `getrlimit`/`setrlimit`/`prlimit64`, `sysinfo`,
//! `times` and `getrusage`.
//!
//! The limits are the kernel's real ones (`crate::limits`: descriptors per
//! process, the user stack, task slots), so a program that sizes a table by
//! `RLIMIT_NOFILE` sizes it right. `RLIMIT_NOFILE` is the one a process can
//! change: `setrlimit` may lower its soft limit and raise it back up to its
//! hard limit (programs routinely "raise" the soft limit to the hard one),
//! and lower the hard limit; the table refuses descriptors past the soft
//! limit (`EMFILE`), for every thread of the process and its children
//! (`task::fdlimit`). Raising the hard limit past the kernel's `fd_max` is
//! `EPERM`. `RLIMIT_CORE` accepts any value (no core file is ever written, so
//! every core limit is honoured). The rest are fixed: a `setrlimit` that asks
//! for exactly the current values succeeds, anything else is `EPERM`, since a
//! limit the kernel would not enforce must not be reported as set.

use crate::task;
use crate::user_ptr;

use super::errno::{err, EFAULT, EINVAL, EPERM, ESRCH};

const RLIM_INFINITY: u64 = u64::MAX;

const RLIMIT_STACK: u32 = 3;
const RLIMIT_CORE: u32 = 4;
const RLIMIT_NPROC: u32 = 6;
const RLIMIT_NOFILE: u32 = 7;
/// `RLIM_NLIMITS`: resources 0..16 exist.
const RLIM_NLIMITS: u32 = 16;

/// `(soft, hard)` for `resource` as process `pid` sees it.
fn limit_of(resource: u32, pid: usize) -> (u64, u64) {
    match resource {
        RLIMIT_STACK => (super::stack_size(), super::stack_size()),
        RLIMIT_CORE => (0, RLIM_INFINITY),
        RLIMIT_NPROC => {
            let max = crate::limits::task_max() as u64;
            (max, max)
        }
        RLIMIT_NOFILE => {
            let (soft, hard) = task::nofile_limits_of(pid).unwrap_or_default();
            (soft as u64, hard as u64)
        }
        _ => (RLIM_INFINITY, RLIM_INFINITY),
    }
}

/// Install `(soft, hard)` as the caller's `resource` (see the module docs):
/// `Ok` to report success, or the errno.
fn install(resource: u32, soft: u64, hard: u64) -> Result<(), u64> {
    if soft > hard {
        return Err(err(EINVAL));
    }
    if resource == RLIMIT_NOFILE {
        let clamp = |value: u64| usize::try_from(value).unwrap_or(usize::MAX);
        return task::set_nofile_limits(clamp(soft), clamp(hard)).map_err(|error| match error {
            task::LimitError::Inverted => err(EINVAL),
            task::LimitError::NotPermitted => err(EPERM),
        });
    }
    if resource == RLIMIT_CORE || (soft, hard) == limit_of(resource, task::linuxstate::tgid()) {
        Ok(())
    } else {
        Err(err(EPERM))
    }
}

fn write_rlimit(ptr: u64, (soft, hard): (u64, u64)) -> Result<(), u64> {
    let ok = user_ptr::try_write::<u64>(ptr, soft).is_ok()
        && user_ptr::try_write_field::<u64>(ptr, 8, hard).is_ok();
    ok.then_some(()).ok_or(err(EFAULT))
}

fn read_rlimit(ptr: u64) -> Result<(u64, u64), u64> {
    match (
        user_ptr::try_read::<u64>(ptr),
        user_ptr::try_read_field::<u64>(ptr, 8),
    ) {
        (Ok(soft), Ok(hard)) => Ok((soft, hard)),
        _ => Err(err(EFAULT)),
    }
}

/// `getrlimit(resource, rlim)`.
pub(super) fn sys_getrlimit(resource: u64, rlim: u64) -> u64 {
    sys_prlimit64(0, resource, 0, rlim)
}

/// `setrlimit(resource, rlim)`.
pub(super) fn sys_setrlimit(resource: u64, rlim: u64) -> u64 {
    sys_prlimit64(0, resource, rlim, 0)
}

/// `prlimit64(pid, resource, new, old)`: `pid` 0 or the caller's own, or
/// another live process whose limits can be read but not changed (that is not
/// ours to do: `EPERM`). The old value is written before the new one is
/// installed.
pub(super) fn sys_prlimit64(pid: u64, resource: u64, new: u64, old: u64) -> u64 {
    let resource = resource as u32;
    if resource >= RLIM_NLIMITS {
        return err(EINVAL);
    }
    let me = task::linuxstate::tgid();
    let own = pid == 0 || pid as usize == me;
    if !own && task::pml4_of(pid as usize).is_none() {
        return err(ESRCH);
    }
    let wanted = if new != 0 {
        match read_rlimit(new) {
            Ok(pair) => Some(pair),
            Err(code) => return code,
        }
    } else {
        None
    };
    if wanted.is_some() && !own {
        return err(EPERM);
    }
    if old != 0 {
        let target = if own { me } else { pid as usize };
        if let Err(code) = write_rlimit(old, limit_of(resource, target)) {
            return code;
        }
    }
    match wanted {
        Some((soft, hard)) => install(resource, soft, hard).err().unwrap_or(0),
        None => 0,
    }
}

/// `sysinfo(info)`: uptime, memory (in 4 KiB units: `mem_unit` = 4096) and
/// the number of tasks. There is no load average (zeros) and no swap.
pub(super) fn sys_sysinfo(info: u64) -> u64 {
    let stats = crate::mem::frame_stats();
    let procs = (1..crate::limits::task_max().min(task::MAX_TASKS))
        .filter(|&slot| task::pml4_of(slot).is_some())
        .count() as u16;
    // struct sysinfo (x86_64): uptime, loads[3], totalram, freeram, sharedram,
    // bufferram, totalswap, freeswap (all 8 bytes), procs (u16), pad,
    // totalhigh, freehigh, mem_unit (u32): 112 bytes.
    let mut out = [0u8; 112];
    let put = |out: &mut [u8; 112], at: usize, value: u64| {
        out[at..at + 8].copy_from_slice(&value.to_le_bytes());
    };
    put(&mut out, 0, task::ticks() / 100);
    put(&mut out, 32, stats.total as u64);
    put(&mut out, 40, stats.free as u64);
    out[80..82].copy_from_slice(&procs.to_le_bytes());
    out[104..108].copy_from_slice(&4096u32.to_le_bytes());
    match user_ptr::try_copy_to(info, &out) {
        Ok(()) => 0,
        Err(_) => err(EFAULT),
    }
}

/// The caller's CPU time in 100 Hz ticks (all of it counted as user time:
/// the kernel does not split user and system time).
fn own_ticks() -> u64 {
    task::cpu_ticks(task::current())
}

/// `times(buf)`: the caller's CPU ticks (children's are not accumulated), and
/// the tick counter as the elapsed real time.
pub(super) fn sys_times(buf: u64) -> u64 {
    if buf != 0 {
        let words = [own_ticks(), 0, 0, 0];
        if user_ptr::try_copy_words(buf, &words).is_err() {
            return err(EFAULT);
        }
    }
    task::ticks()
}

/// `getrusage(who, usage)`: `RUSAGE_SELF`/`RUSAGE_THREAD` report the caller's
/// CPU time as `ru_utime`; `RUSAGE_CHILDREN` reports zeros (no accumulation of
/// reaped children yet). Every other field is zero.
pub(super) fn sys_getrusage(who: u64, usage: u64) -> u64 {
    const RUSAGE_SELF: i64 = 0;
    const RUSAGE_CHILDREN: i64 = -1;
    const RUSAGE_THREAD: i64 = 1;
    let ticks = match who as i64 {
        RUSAGE_SELF | RUSAGE_THREAD => own_ticks(),
        RUSAGE_CHILDREN => 0,
        _ => return err(EINVAL),
    };
    let mut out = [0u8; 144];
    out[0..8].copy_from_slice(&(ticks / 100).to_le_bytes());
    out[8..16].copy_from_slice(&((ticks % 100) * 10_000).to_le_bytes());
    match user_ptr::try_copy_to(usage, &out) {
        Ok(()) => 0,
        Err(_) => err(EFAULT),
    }
}
