//! `sched_setattr` (314) and `sched_getattr` (315), and the policy that
//! `sched_getscheduler` reports.
//!
//! LazyOS schedules by class and stride weight ([`crate::task::nice`]), not by
//! Linux policies, so the calls do what can be honoured and refuse the rest:
//!
//! * `SCHED_OTHER`, `SCHED_BATCH` and `SCHED_IDLE` are accepted. The nice value
//!   becomes the task's weight (`SCHED_IDLE` the smallest one); the policy and
//!   nice value are kept and read back by `sched_getattr`. BATCH and OTHER
//!   differ only in the name; `SCHED_IDLE` ignores the attr's nice value and
//!   keeps the task's own, as Linux does. Lowering a nice value, leaving `SCHED_IDLE` or
//!   clearing `reset_on_fork` takes privilege, as on Linux with
//!   `RLIMIT_NICE` 0.
//! * `SCHED_FIFO`, `SCHED_RR` and `SCHED_DEADLINE` are `EPERM` without
//!   privilege, as on Linux without `CAP_SYS_NICE`. With it they are `EINVAL`:
//!   LazyOS has no such policy to give, and `sched_get_priority_max` says so.
//! * The utilization-clamp flags are `EOPNOTSUPP` (a Linux kernel built
//!   without `CONFIG_UCLAMP_TASK` answers the same).
//! * `sched_setscheduler`, `sched_setparam`, `setpriority` and `getpriority`
//!   are not offered (`ENOSYS`), so this is the only way to change a task's
//!   policy or nice value and `sched_getattr` and `sched_getscheduler` cannot
//!   disagree with another setter. Adding one means routing it through
//!   [`decide`] and [`task::sched_update`].
//! * `pid` is a thread id (a task slot) checked on all 64 bits ([`target`]).
//!   Without privilege a caller may change only tasks of its own process
//!   (same thread-group leader) and uid: a native service task that shares the
//!   uid of a session is not its to re-weight. Reading is open.
//!
//! Privilege is `CAP_SYS_ADMIN` (LazyOS has no `CAP_SYS_NICE`; nobody is root
//! and a session user holds no capability). The size handling is Linux's:
//! `E2BIG` for a `sched_attr` that is too small or has unknown non-zero tail
//! bytes, with the size it knows (56) written back in every case, and
//! `sched_getattr` writes `min(size, 56)` bytes, never more.

use crate::ipc::credentials::{self, CAP_SYS_ADMIN};
use crate::task::{self, linuxstate::SchedState, nice, SchedTarget, UpdateError};
use crate::user_ptr;
use alloc::vec;

use super::errno::{err, E2BIG, EFAULT, EINVAL, EOPNOTSUPP, EPERM, ESRCH};

const SCHED_OTHER: u32 = 0;
const SCHED_FIFO: u32 = 1;
const SCHED_RR: u32 = 2;
const SCHED_BATCH: u32 = 3;
const SCHED_IDLE: u32 = 5;
const SCHED_DEADLINE: u32 = 6;

const FLAG_RESET_ON_FORK: u64 = 0x01;
const FLAG_RECLAIM: u64 = 0x02;
const FLAG_DL_OVERRUN: u64 = 0x04;
const FLAG_KEEP_POLICY: u64 = 0x08;
const FLAG_KEEP_PARAMS: u64 = 0x10;
const FLAG_UTIL_CLAMP: u64 = 0x20 | 0x40;
const FLAG_ALL: u64 = 0x7f;

/// `sizeof(struct sched_attr)` in the first version, and now.
const SIZE_VER0: usize = 48;
const SIZE_NOW: usize = 56;
const PAGE: usize = 4096;
/// Highest real-time priority Linux accepts.
const MAX_RT_PRIO: u32 = 99;

/// The fields of `struct sched_attr` the calls look at.
struct Attr {
    policy: u32,
    flags: u64,
    nice: i32,
    priority: u32,
    runtime: u64,
    deadline: u64,
    period: u64,
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap_or_default())
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap_or_default())
}

/// Read the caller's `sched_attr` (`Err` is the errno to return), as
/// Linux's `sched_copy_attr` does.
fn read_attr(uattr: u64) -> Result<Attr, u64> {
    let Ok(declared) = user_ptr::try_read::<u32>(uattr) else {
        return Err(err(EFAULT));
    };
    // A zero size means the first version's.
    let size = if declared == 0 {
        SIZE_VER0
    } else {
        declared as usize
    };
    if !(SIZE_VER0..=PAGE).contains(&size) {
        let _ = user_ptr::try_write::<u32>(uattr, SIZE_NOW as u32);
        return Err(err(E2BIG));
    }
    let known = size.min(SIZE_NOW);
    let mut raw = [0u8; SIZE_NOW];
    match user_ptr::try_bytes(uattr, known) {
        Ok(bytes) => raw[..known].copy_from_slice(bytes),
        Err(_) => return Err(err(EFAULT)),
    }
    // A newer struct is fine only when what this kernel does not know is zero.
    if size > SIZE_NOW {
        match user_ptr::try_bytes(uattr + SIZE_NOW as u64, size - SIZE_NOW) {
            Ok(tail) if tail.iter().all(|&b| b == 0) => {}
            Ok(_) => {
                let _ = user_ptr::try_write::<u32>(uattr, SIZE_NOW as u32);
                return Err(err(E2BIG));
            }
            Err(_) => return Err(err(EFAULT)),
        }
    }
    Ok(Attr {
        policy: le32(&raw, 4),
        flags: le64(&raw, 8),
        nice: le32(&raw, 16) as i32,
        priority: le32(&raw, 20),
        runtime: le64(&raw, 24),
        deadline: le64(&raw, 32),
        period: le64(&raw, 40),
    })
}

/// Linux's `__checkparam_dl`, for the part that is about the numbers.
fn deadline_params_ok(attr: &Attr) -> bool {
    const MIN_RUNTIME: u64 = 1 << 10;
    let period = if attr.period == 0 {
        attr.deadline
    } else {
        attr.period
    };
    attr.deadline != 0
        && attr.runtime >= MIN_RUNTIME
        && attr.runtime <= attr.deadline
        && attr.deadline <= period
}

fn privileged() -> bool {
    credentials::of(task::current()).has_cap(CAP_SYS_ADMIN)
}

/// Largest pid the calls accept (`pid_t` is an `int`; the register holds the
/// rest of the 64 bits, which must not be dropped: 1 << 32 is not the caller).
const PID_MAX: u64 = i32::MAX as u64;

/// The slot `pid` names (0 is the caller), or the errno: `EINVAL` for a value
/// that is no `pid_t` (checked on all 64 bits, before any lookup), `ESRCH` for
/// a slot with no task. A tid is the task's slot, as `gettid` reports it.
pub(super) fn target(pid: u64) -> Result<usize, u64> {
    if pid > PID_MAX {
        return Err(err(EINVAL));
    }
    let slot = if pid == 0 {
        task::current()
    } else {
        pid as usize
    };
    if task::priority(slot).is_none() {
        return Err(err(ESRCH));
    }
    Ok(slot)
}

/// The policy and flags `sched_getscheduler` reports for `slot` (`None` when
/// the slot holds no task).
pub(super) fn policy_of(slot: usize) -> Option<u64> {
    const RESET_ON_FORK: u64 = 0x4000_0000;
    let state = task::sched_read(slot)?;
    Some(
        u64::from(state.policy)
            | if state.reset_on_fork {
                RESET_ON_FORK
            } else {
                0
            },
    )
}

/// `sched_setattr(pid, attr, flags)`.
pub(super) fn sys_sched_setattr(pid: u64, uattr: u64, flags: u64) -> u64 {
    if uattr == 0 || pid > PID_MAX || flags as u32 != 0 {
        return err(EINVAL);
    }
    let attr = match read_attr(uattr) {
        Ok(attr) => attr,
        Err(errno) => return errno,
    };
    let slot = match target(pid) {
        Ok(slot) => slot,
        Err(errno) => return errno,
    };
    match apply(slot, &attr) {
        Ok(()) => 0,
        Err(errno) => err(errno),
    }
}

/// Who is asking, as far as the rules go.
struct Caller {
    allowed: bool,
    uid: u32,
    /// The caller's thread-group leader (`getpid`).
    leader: usize,
}

/// Validate and apply `attr` to `slot`; the errno on refusal. The decision,
/// the stored state and the weight are one step under the task-table lock
/// ([`task::sched_update`]), so no other thread's change can fall between the
/// checks and the store.
fn apply(slot: usize, attr: &Attr) -> Result<(), u64> {
    let caller = Caller {
        allowed: privileged(),
        uid: credentials::of(task::current()).uid,
        leader: task::linuxstate::tgid(),
    };
    match task::sched_update(slot, |target| decide(attr, &caller, &target)) {
        Ok(()) => Ok(()),
        Err(UpdateError::NoSuchTask) => Err(ESRCH),
        Err(UpdateError::Refused(errno)) => Err(errno),
    }
}

/// The state `attr` asks `target` to have, or the errno that refuses it. Runs
/// under the task-table lock: pure, no allocation.
fn decide(attr: &Attr, caller: &Caller, target: &SchedTarget) -> Result<SchedState, u64> {
    let old = target.state;
    if attr.flags & !FLAG_ALL != 0 {
        return Err(EINVAL);
    }
    let policy = if attr.flags & FLAG_KEEP_POLICY != 0 {
        u32::from(old.policy)
    } else {
        attr.policy
    };
    if !matches!(
        policy,
        SCHED_OTHER | SCHED_FIFO | SCHED_RR | SCHED_BATCH | SCHED_IDLE | SCHED_DEADLINE
    ) {
        return Err(EINVAL);
    }
    let realtime = matches!(policy, SCHED_FIFO | SCHED_RR);
    let deadline = policy == SCHED_DEADLINE;
    if (deadline && !deadline_params_ok(attr))
        || (!deadline && attr.flags & (FLAG_RECLAIM | FLAG_DL_OVERRUN) != 0)
        || realtime != (attr.priority != 0)
        || (realtime && attr.priority > MAX_RT_PRIO)
    {
        return Err(EINVAL);
    }
    if attr.flags & FLAG_UTIL_CLAMP != 0 {
        return Err(EOPNOTSUPP);
    }
    let reset_on_fork = attr.flags & FLAG_RESET_ON_FORK != 0;
    let level = attr.nice.clamp(nice::MIN_NICE, nice::MAX_NICE);
    let keep_params = attr.flags & FLAG_KEEP_PARAMS != 0;
    // As in Linux's `__sched_setscheduler`, a nice value belongs to the fair
    // policies only: `SCHED_IDLE` neither applies `sched_nice` nor needs
    // privilege for it (a zeroed attr, which `chrt --idle` sends, must work).
    let new_nice = if keep_params || policy == SCHED_IDLE {
        i32::from(old.nice)
    } else {
        level
    };

    if realtime || deadline {
        return Err(if caller.allowed { EINVAL } else { EPERM });
    }
    let raises = new_nice < i32::from(old.nice);
    let leaves_idle = old.is_idle() && policy != SCHED_IDLE;
    let unsets_reset = old.reset_on_fork && !reset_on_fork;
    // Without privilege only the caller's own process is its to re-weight: a
    // native service task that happens to share the uid is not.
    let foreign = target.cred.uid != caller.uid || target.leader != caller.leader;
    if !caller.allowed && (foreign || raises || leaves_idle || unsets_reset) {
        return Err(EPERM);
    }
    Ok(SchedState {
        policy: policy as u8,
        reset_on_fork,
        nice: new_nice as i8,
    })
}

/// `sched_getattr(pid, attr, size, flags)`.
pub(super) fn sys_sched_getattr(pid: u64, uattr: u64, size: u64, flags: u64) -> u64 {
    let size = size as usize;
    if uattr == 0 || pid > PID_MAX || !(SIZE_VER0..=PAGE).contains(&size) || flags as u32 != 0 {
        return err(EINVAL);
    }
    let slot = match target(pid) {
        Ok(slot) => slot,
        Err(errno) => return errno,
    };
    let Some(state) = task::sched_read(slot) else {
        return err(ESRCH);
    };
    let written = size.min(SIZE_NOW);
    let mut raw = vec![0u8; written];
    raw[0..4].copy_from_slice(&(written as u32).to_le_bytes());
    raw[4..8].copy_from_slice(&u32::from(state.policy).to_le_bytes());
    let flags = if state.reset_on_fork {
        FLAG_RESET_ON_FORK
    } else {
        0
    };
    raw[8..16].copy_from_slice(&flags.to_le_bytes());
    raw[16..20].copy_from_slice(&i32::from(state.nice).to_le_bytes());
    match user_ptr::try_copy_to(uattr, &raw) {
        Ok(()) => 0,
        Err(_) => err(EFAULT),
    }
}
