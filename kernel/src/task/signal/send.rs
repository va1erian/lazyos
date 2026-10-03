//! Sending signals: the `kill`/`tkill`/`tgkill` targeting rules and the
//! permission check every sender goes through (issue #230).
//!
//! POSIX lets a sender signal a process only when its credentials match the
//! target's (or it holds the kill capability). The check lives in
//! [`may_signal`] and is applied by `send_to_slot`, the single sink, so no
//! entry point (pid, tid, group, everyone) can skip it.

use super::{
    send_checked, send_to_slot, slot_info, Expect, SigInfo, SignalError, SlotList, NSIG, SIGCONT,
};
use crate::ipc::credentials::{self, CAP_KILL};
use crate::task::{process, TaskState, KERNEL_TASK, MAX_TASKS, TASKS};

/// The first user task the kernel starts is `init`; `kill(-1)` never reaches
/// it (Linux exempts pid 1 from broadcast signals).
const INIT_TASK: usize = 1;

/// Whether `caller` may send `sig` to `target`.
///
/// Allowed: itself, the kernel task, a target whose uid equals the caller's,
/// a holder of `CAP_KILL`, and `SIGCONT` within one login session (Linux's
/// job-control exception).
pub(super) fn may_signal(caller: usize, target: usize, sig: u8) -> bool {
    if caller == target || caller == KERNEL_TASK {
        return true;
    }
    let sender = credentials::of(caller);
    let victim = credentials::of(target);
    if sender.has_cap(CAP_KILL) || sender.uid == victim.uid {
        return true;
    }
    sig == SIGCONT && sender.session != 0 && sender.session == victim.session
}

/// `kill(pid, sig)` with Linux's pid encoding: positive is a pid, 0 is the
/// caller's process group, -1 is every process except init, and other negatives
/// are a process group.
pub fn kill(caller: usize, pid: i64, sig: u8, info: SigInfo) -> Result<(), SignalError> {
    if sig as usize >= NSIG {
        return Err(SignalError::Invalid);
    }
    match pid {
        0 => kill_group(caller, process::pgid_of(caller), sig, info),
        -1 => kill_all(caller, sig, info),
        // `unsigned_abs` keeps `i64::MIN` from overflowing the negation.
        pid if pid < 0 => {
            let pgid =
                usize::try_from(pid.unsigned_abs()).map_err(|_| SignalError::NoSuchProcess)?;
            kill_group(caller, pgid, sig, info)
        }
        pid => {
            let target = usize::try_from(pid).map_err(|_| SignalError::NoSuchProcess)?;
            if target == KERNEL_TASK {
                return if sig == 0 {
                    Ok(())
                } else {
                    Err(SignalError::NotPermitted)
                };
            }
            if slot_info(target).is_none() {
                return Err(SignalError::NoSuchProcess);
            }
            send_to_slot(caller, target, sig, info)
        }
    }
}

/// `tkill(tid, sig)` / `tgkill(tgid, tid, sig)` after the shim validated `tgid`.
pub fn send_tid(caller: usize, tid: usize, sig: u8, info: SigInfo) -> Result<(), SignalError> {
    if sig as usize >= NSIG {
        return Err(SignalError::Invalid);
    }
    if tid == KERNEL_TASK || slot_info(tid).is_none() {
        return Err(SignalError::NoSuchProcess);
    }
    send_to_slot(caller, tid, sig, info)
}

/// Send to every slot in `targets`, Linux-style: success if any delivery
/// worked, otherwise `EPERM` if any target was off limits, else the last error.
fn send_many(caller: usize, targets: &SlotList, sig: u8, info: SigInfo) -> Result<(), SignalError> {
    let mut delivered = false;
    let mut failure = SignalError::NoSuchProcess;
    for target in targets.iter() {
        match send_to_slot(caller, target, sig, info) {
            Ok(()) => delivered = true,
            Err(SignalError::NotPermitted) => failure = SignalError::NotPermitted,
            Err(error) if failure != SignalError::NotPermitted => failure = error,
            Err(_) => {}
        }
    }
    if delivered {
        Ok(())
    } else {
        Err(failure)
    }
}

/// `kill(-pgid, sig)`: every task in the group, including the caller's group.
fn kill_group(caller: usize, pgid: usize, sig: u8, info: SigInfo) -> Result<(), SignalError> {
    let mut targets = SlotList::new();
    {
        let tasks = TASKS.lock();
        for slot in 1..MAX_TASKS {
            if tasks[slot]
                .as_ref()
                .is_some_and(|task| task.pgid == pgid && task.state != TaskState::Done)
            {
                targets.push(slot);
            }
        }
    }
    if targets.len == 0 {
        return Err(SignalError::NoSuchProcess);
    }
    send_many(caller, &targets, sig, info)
}

/// A terminal's own signal (`^C`, `SIGWINCH`, the hang-up) for its
/// foreground group: the kernel sends it to the tasks of `pgid` in session
/// `sid` only, each re-checked at delivery ([`Expect`]), so a group id or a
/// slot recycled by another session never receives it.
pub fn kill_terminal_group(pgid: usize, sid: usize, sig: u8) -> Result<(), SignalError> {
    if sig as usize >= NSIG {
        return Err(SignalError::Invalid);
    }
    let expect = Expect { pgid, sid };
    let mut targets = SlotList::new();
    {
        let tasks = TASKS.lock();
        for slot in 1..MAX_TASKS {
            if tasks[slot].as_ref().is_some_and(|task| {
                task.pgid == pgid && task.sid == sid && task.state != TaskState::Done
            }) {
                targets.push(slot);
            }
        }
    }
    let mut delivered = false;
    for target in targets.iter() {
        let info = SigInfo::kernel();
        delivered |= send_checked(KERNEL_TASK, target, sig, info, Some(expect)).is_ok();
    }
    if delivered {
        Ok(())
    } else {
        Err(SignalError::NoSuchProcess)
    }
}

/// `kill(-1, sig)`: everything but init, the kernel and the caller's own
/// process (Linux skips the calling thread group so `kill(-1, SIGKILL)` does
/// not take its sender down first).
fn kill_all(caller: usize, sig: u8, info: SigInfo) -> Result<(), SignalError> {
    let caller_pml4 = slot_info(caller).map(|(pml4, _)| pml4);
    let mut targets = SlotList::new();
    {
        let tasks = TASKS.lock();
        for slot in (INIT_TASK + 1)..MAX_TASKS {
            let Some(task) = tasks[slot].as_ref() else {
                continue;
            };
            if task.state != TaskState::Done && Some(task.pml4) != caller_pml4 {
                targets.push(slot);
            }
        }
    }
    if targets.len == 0 {
        return Err(SignalError::NoSuchProcess);
    }
    send_many(caller, &targets, sig, info)
}
