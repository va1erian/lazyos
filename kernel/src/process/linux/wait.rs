//! `wait4` and `waitid`: collect a finished child.
//!
//! The `pid` argument selects which children qualify, as on Linux: a positive
//! pid is that child only, `-1` is any child, `0` the caller's process group
//! and `< -1` the group `-pid`. A child killed by a signal is reported as
//! `WIFSIGNALED` with that signal (`wstatus` low 7 bits, the core-dump bit for
//! the signals whose default action dumps core), any other as `WIFEXITED` with
//! its exit code. Stopped and continued children are not reported
//! (`WUNTRACED`/`WCONTINUED` are accepted and change nothing); `rusage` is
//! zeroed when asked for, since the kernel keeps no per-child accounting yet.

use crate::task::{self, ChildFilter, WakeReason};
use crate::user_ptr;

use super::errno::{err, ECHILD, EFAULT, EINTR, EINVAL};

const WNOHANG: u64 = 1;
const WUNTRACED: u64 = 2;
const WEXITED: u64 = 4;
const WCONTINUED: u64 = 8;
const WNOWAIT: u64 = 0x0100_0000;
/// `__WNOTHREAD`, `__WALL`, `__WCLONE`: thread-selection modifiers. LazyOS
/// threads are never children, so they change nothing.
const W_THREAD_BITS: u64 = 0x2000_0000 | 0x4000_0000 | 0x8000_0000;

/// `struct rusage` is 144 bytes on x86_64.
const RUSAGE_SIZE: usize = 144;

/// Signals whose default action dumps core (`WCOREDUMP`).
fn dumps_core(sig: u8) -> bool {
    matches!(sig, 3 | 4 | 5 | 6 | 7 | 8 | 11 | 24 | 25 | 31)
}

/// The `wstatus` word for a reaped child.
pub(super) fn wait_status(code: u64, signal: u8) -> u32 {
    if signal != 0 {
        u32::from(signal & 0x7f) | if dumps_core(signal) { 0x80 } else { 0 }
    } else {
        ((code & 0xff) as u32) << 8
    }
}

/// The children `pid` names, from the caller's point of view.
fn filter_for(pid: i64) -> ChildFilter {
    match pid {
        -1 => ChildFilter::Any,
        0 => ChildFilter::Group(task::pgid()),
        pid if pid < -1 => ChildFilter::Group(pid.unsigned_abs() as usize),
        pid => ChildFilter::Pid(pid as usize),
    }
}

/// Park until a qualifying child is reapable (or `WNOHANG`). Returns the
/// reaped `(pid, wstatus)`, `None` for a `WNOHANG` call that found nothing, or
/// the errno.
fn wait_for(filter: ChildFilter, nohang: bool) -> Result<Option<(usize, u32)>, u64> {
    if !task::has_child_filtered(filter) {
        return Err(err(ECHILD));
    }
    loop {
        if let Some((slot, code, signal)) = task::reap_child_filtered(filter) {
            return Ok(Some((slot, wait_status(code, signal))));
        }
        if nohang {
            return Ok(None);
        }
        // No child is reapable yet: park until one exits. The recheck above
        // runs with interrupts off, so an exit cannot slip in between.
        match task::wait_child_exit() {
            WakeReason::Woken | WakeReason::TimedOut => {}
            WakeReason::Interrupted => return Err(err(EINTR)),
        }
    }
}

/// `wait4(pid, wstatus, options, rusage)`.
pub(super) fn sys_wait4(pid: u64, status: u64, options: u64, rusage: u64) -> u64 {
    if options & !(WNOHANG | WUNTRACED | WCONTINUED | W_THREAD_BITS) != 0 {
        return err(EINVAL);
    }
    let pid = pid as u32 as i32 as i64;
    match wait_for(filter_for(pid), options & WNOHANG != 0) {
        Ok(Some((slot, wstatus))) => {
            if status != 0 && user_ptr::try_write::<u32>(status, wstatus).is_err() {
                return err(EFAULT);
            }
            if rusage != 0 && user_ptr::try_copy_to(rusage, &[0u8; RUSAGE_SIZE]).is_err() {
                return err(EFAULT);
            }
            slot as u64
        }
        Ok(None) => 0,
        Err(code) => code,
    }
}

/// `waitid(idtype, id, infop, options, rusage)`: `P_ALL`, `P_PID`, `P_PGID`;
/// `WEXITED` is required (stops and continues are never reported), `WNOWAIT`
/// is refused (a reaped child cannot be left waitable).
pub(super) fn sys_waitid(idtype: u64, id: u64, infop: u64, options: u64, rusage: u64) -> u64 {
    const P_ALL: u64 = 0;
    const P_PID: u64 = 1;
    const P_PGID: u64 = 2;
    const CLD_EXITED: i32 = 1;
    const CLD_KILLED: i32 = 2;
    const CLD_DUMPED: i32 = 3;
    const SIGCHLD: i32 = 17;
    if options & WEXITED == 0 || options & WNOWAIT != 0 {
        return err(EINVAL);
    }
    let filter = match idtype {
        P_ALL => ChildFilter::Any,
        P_PID => ChildFilter::Pid(id as usize),
        P_PGID if id == 0 => ChildFilter::Group(task::pgid()),
        P_PGID => ChildFilter::Group(id as usize),
        _ => return err(EINVAL),
    };
    let reaped = match wait_for(filter, options & WNOHANG != 0) {
        Ok(reaped) => reaped,
        Err(code) => return code,
    };
    // siginfo_t: si_signo, si_errno, si_code, pad, si_pid, si_uid, si_status.
    let mut info = [0u8; 128];
    if let Some((slot, wstatus)) = reaped {
        let (code, value) = match wstatus & 0x7f {
            0 => (CLD_EXITED, ((wstatus >> 8) & 0xff) as i32),
            sig if wstatus & 0x80 != 0 => (CLD_DUMPED, sig as i32),
            sig => (CLD_KILLED, sig as i32),
        };
        info[0..4].copy_from_slice(&SIGCHLD.to_le_bytes());
        info[8..12].copy_from_slice(&code.to_le_bytes());
        info[16..20].copy_from_slice(&(slot as i32).to_le_bytes());
        info[24..28].copy_from_slice(&value.to_le_bytes());
    }
    if infop != 0 && user_ptr::try_copy_to(infop, &info).is_err() {
        return err(EFAULT);
    }
    if rusage != 0 && user_ptr::try_copy_to(rusage, &[0u8; RUSAGE_SIZE]).is_err() {
        return err(EFAULT);
    }
    0
}
