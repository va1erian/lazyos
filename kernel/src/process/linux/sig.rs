//! Signal syscalls (issue #60): `rt_sigaction`, `rt_sigprocmask`,
//! `rt_sigreturn`, `sigaltstack`, `kill`, `tkill`, and `tgkill`. The actual
//! disposition table, pending/blocked masks, and delivery onto the user stack
//! live in [`crate::task::signal`]; this module is only the ABI-facing
//! encode/decode of its types plus the three ways to target a signal at a
//! pid, a tid, or a `(tgid, tid)` pair.

use crate::task;
use crate::task::signal::{self, Disposition, SignalError};

use super::errno::{err, EINVAL, EPERM, ESRCH};
use super::uaccess::{read_u32, read_u64, write_u32, write_u64};

/// Map a signal-layer failure to its Linux errno.
fn signal_err(error: SignalError) -> u64 {
    match error {
        SignalError::NoSuchProcess => err(ESRCH),
        SignalError::NotPermitted => err(EPERM),
        SignalError::Invalid => err(EINVAL),
    }
}

/// Kernel `struct sigaction` <-> [`Disposition`]. The layout musl passes is
/// `{ handler, flags, restorer, mask }` (32 bytes on x86_64).
fn disposition_to_kernel(disposition: Disposition) -> (u64, u64, u64, u64) {
    match disposition {
        Disposition::Default => (signal::SIG_DFL, 0, 0, 0),
        Disposition::Ignore => (signal::SIG_IGN, 0, 0, 0),
        Disposition::Handler {
            handler,
            flags,
            restorer,
            mask,
        } => (handler, flags, restorer, mask),
    }
}

/// `rt_sigaction(sig, act, oldact, sigsetsize)`. `SIGKILL`/`SIGSTOP` are
/// refused with `EINVAL`, like Linux: they cannot be caught or ignored.
pub(super) fn sys_rt_sigaction(sig: u64, act: u64, oldact: u64, sigsetsize: u64) -> u64 {
    if sig == 0 || sig as usize >= signal::NSIG {
        return err(EINVAL);
    }
    if sigsetsize != 0 && sigsetsize != 8 {
        return err(EINVAL);
    }
    let me = task::current();
    let sig = sig as u8;
    if oldact != 0 {
        let (handler, flags, restorer, mask) = disposition_to_kernel(signal::action(me, sig));
        write_u64(oldact, handler);
        write_u64(oldact + 8, flags);
        write_u64(oldact + 16, restorer);
        // The kernel stores `sa_mask` in its internal bit order.
        write_u64(oldact + 24, signal::kernel_to_linux_sigset(mask));
    }
    if act != 0 {
        let handler = read_u64(act);
        let flags = read_u64(act + 8);
        let restorer = read_u64(act + 16);
        // User space passes a Linux `sigset_t` (bit `sig - 1`).
        let mask = signal::linux_sigset_to_kernel(read_u64(act + 24));
        let disposition = match handler {
            signal::SIG_DFL => Disposition::Default,
            signal::SIG_IGN => Disposition::Ignore,
            handler => Disposition::Handler {
                handler,
                flags,
                restorer,
                // `SIGKILL`/`SIGSTOP` are never blockable, so they can never
                // be part of a handler mask either.
                mask: mask & !(1 << signal::SIGKILL | 1 << signal::SIGSTOP),
            },
        };
        if let Err(error) = signal::set_action(me, sig, disposition) {
            return signal_err(error);
        }
    }
    0
}

/// `rt_sigprocmask(how, set, oldset, sigsetsize)`. `SIGKILL`/`SIGSTOP` bits are
/// silently discarded: POSIX says attempts to block them are ignored.
pub(super) fn sys_rt_sigprocmask(how: u64, set: u64, oldset: u64, sigsetsize: u64) -> u64 {
    if sigsetsize != 0 && sigsetsize != 8 {
        return err(EINVAL);
    }
    let me = task::current();
    // The kernel keeps its blocked set in internal bit order; user space
    // passes and reads Linux `sigset_t` (bit `sig - 1`).
    let current = signal::blocked(me);
    if set == 0 {
        if oldset != 0 {
            write_u64(oldset, signal::kernel_to_linux_sigset(current));
        }
        return 0;
    }
    if how != signal::SIG_BLOCK && how != signal::SIG_UNBLOCK && how != signal::SIG_SETMASK {
        return err(EINVAL);
    }
    if oldset != 0 {
        write_u64(oldset, signal::kernel_to_linux_sigset(current));
    }
    let requested = signal::linux_sigset_to_kernel(read_u64(set));
    let next = match how {
        signal::SIG_BLOCK => current | requested,
        signal::SIG_UNBLOCK => current & !requested,
        signal::SIG_SETMASK => requested,
        _ => return err(EINVAL),
    };
    signal::set_blocked(me, next);
    0
}

/// `rt_sigreturn()`: the restorer (`__restore_rt`) issues this syscall with RSP
/// just past the `pretcode` slot of the frame `deliver_linux` built. Restore
/// the interrupted registers from the `ucontext_t` and make `sysretq` land
/// there, returning the frame's `rax`. The frame is user-controlled, so a
/// forged one (non-user `rip`/`rsp`) kills the task with `SIGSEGV` instead of
/// reaching `sysretq` (#221); the flags are sanitised on the way out.
pub(super) fn sys_rt_sigreturn() -> u64 {
    let user_rsp = crate::arch::linux::saved_user_rsp();
    let Some((regs, mask)) = signal::restore_frame(user_rsp) else {
        signal::die_with_segv();
    };
    signal::set_blocked(task::current(), mask);
    signal::apply_linux_frame_syscall(&regs);
    regs.rax
}

/// `sigaltstack(ss, old_ss)`: install/disable/report the alternate signal
/// stack. The frame lands there when the action carries `SA_ONSTACK`.
pub(super) fn sys_sigaltstack(ss: u64, old_ss: u64) -> u64 {
    let me = task::current();
    if old_ss != 0 {
        let current = signal::altstack(me);
        write_u64(old_ss, current.sp);
        write_u32(
            old_ss + 8,
            if current.enabled {
                0
            } else {
                signal::SS_DISABLE
            },
        );
        write_u64(old_ss + 16, current.size);
    }
    if ss != 0 {
        let sp = read_u64(ss);
        let flags = read_u32(ss + 8);
        let size = read_u64(ss + 16);
        if flags & signal::SS_ONSTACK != 0 {
            return err(EINVAL); // cannot set a stack marked as in use
        }
        let stack = if flags & signal::SS_DISABLE != 0 {
            signal::AltStack::DISABLED
        } else {
            signal::AltStack {
                sp,
                size,
                enabled: true,
            }
        };
        if let Err(error) = signal::set_altstack(me, stack) {
            return signal_err(error);
        }
    }
    0
}

/// `kill(pid, sig)`: pid > 0 targets one process, 0 the caller's process
/// group, -1 everyone but init, and pid < -1 the group `-pid`.
pub(super) fn sys_kill(pid: u64, sig: u64) -> u64 {
    let me = task::current();
    let info = signal::SigInfo::user(me, signal::SI_USER);
    match signal::kill(me, pid as i64, sig as u8, info) {
        Ok(()) => 0,
        Err(error) => signal_err(error),
    }
}

/// `tkill(tid, sig)`: send to one thread of the caller's process.
pub(super) fn sys_tkill(tid: u64, sig: u64) -> u64 {
    let me = task::current();
    let info = signal::SigInfo::user(me, signal::SI_TKILL);
    match signal::send_tid(me, tid as usize, sig as u8, info) {
        Ok(()) => 0,
        Err(error) => signal_err(error),
    }
}

/// `tgkill(tgid, tid, sig)`: like `tkill`, but the thread group must match.
pub(super) fn sys_tgkill(tgid: u64, tid: u64, sig: u64) -> u64 {
    if signal::tgid_of(tid as usize) != tgid as usize {
        return err(ESRCH);
    }
    sys_tkill(tid, sig)
}
