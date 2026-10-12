//! Which task `sched_setattr`/`sched_getattr`/`sched_getscheduler`/
//! `sched_getparam` name (review of #819): the pid is a task slot checked on
//! all 64 bits, an unprivileged caller may change only its own process, a
//! dead slot is `ESRCH`, and a `SCHED_IDLE` or nice value set while the CPU
//! guard has demoted the task takes effect when the window ends.

use super::schedattr::*;
use super::*;
use crate::ipc::credentials::{self, Cred};
use crate::task::guard::WINDOW_TICKS;
use crate::task::PriorityClass;

const GETPARAM: u64 = 143;
const SETSCHEDULER: u64 = 144;
const ESRCH: u64 = neg(3);

/// `sched_getparam(pid, &mut priority)`.
fn getparam(pid: u64) -> u64 {
    let mut priority = 0i32;
    sys(GETPARAM, &[pid, &mut priority as *mut i32 as u64])
}

/// Every call that names a task, against `pid`: the result of each.
fn named(pid: u64) -> [(&'static str, u64); 4] {
    let ok = attr(OTHER, 0);
    let mut out = [0u8; 56];
    [
        (
            "sched_setattr",
            sys(SETATTR, &[pid, &ok as *const Attr as u64, 0]),
        ),
        (
            "sched_getattr",
            sys(GETATTR, &[pid, out.as_mut_ptr() as u64, 56, 0]),
        ),
        ("sched_getscheduler", sys(GETSCHEDULER, &[pid])),
        ("sched_getparam", getparam(pid)),
    ]
}

/// A pid is checked as the 64-bit value it is: one that is no `pid_t` is
/// `EINVAL` before anything is looked up, even when its low 32 bits name a
/// live task (or the caller, 0).
pub fn schedattr_pid_is_checked_whole() -> Result<(), String> {
    scoped(ADMIN, || {
        let me = task::current() as u64;
        let bad = [
            1u64 << 32,
            2 << 32,
            (1 << 32) | me,
            (1 << 63) | me,
            u64::from(u32::MAX),
            u64::from(i32::MAX as u32) + 1,
            u64::MAX,
        ];
        for pid in bad {
            for (call, got) in named(pid) {
                check!(got == EINVAL, "{call}({pid:#x}) returned {got:#x}");
            }
        }
        // The largest pid_t is a valid pid; there is no such task.
        for (call, got) in named(i32::MAX as u64) {
            check!(got == ESRCH, "{call}(INT_MAX) returned {got:#x}");
        }
        // And the plain forms still work.
        for pid in [0, me] {
            for (call, got) in named(pid) {
                check!(got == 0, "{call}({pid}) returned {got:#x}");
            }
        }
        Ok(())
    })
}

/// A slot with no task, one past the table, and a task that was reaped are
/// all `ESRCH` for every call; the attributes read back for a live task do
/// not leak to the next occupant of its slot.
pub fn schedattr_dead_slot_is_esrch() -> Result<(), String> {
    scoped(ADMIN, || {
        let child = task::spawn_fork().map_err(|error| format!("fork: {error}"))?;
        check!(
            setattr(child as u64, &attr(IDLE, 7)) == 0,
            "set on the child"
        );
        check!(
            getattr(child as u64)?.policy == IDLE,
            "read back from the child"
        );
        task::harness::finish(child, 0);
        check!(task::reap_child().is_some(), "the child was not reapable");
        let empty = (1..task::MAX_TASKS)
            .find(|&slot| task::priority(slot).is_none())
            .ok_or("no empty slot")?;
        for pid in [
            child as u64,
            empty as u64,
            task::MAX_TASKS as u64,
            900,
            100_000,
        ] {
            for (call, got) in named(pid) {
                check!(got == ESRCH, "{call}({pid}) returned {got:#x}");
            }
        }
        // A new task (likely in the dead one's slot) starts from its parent's
        // attributes, not the dead task's.
        let again = task::spawn_fork().map_err(|error| format!("fork: {error}"))?;
        let read = getattr(again as u64)?;
        check!(
            read.policy == OTHER && read.nice == 0,
            "a new task in slot {again} (dead one {child}) has policy {} nice {}",
            read.policy,
            read.nice
        );
        Ok(())
    })
}

/// An unprivileged caller changes only its own process: another process of the
/// same user (a native service would be one) is `EPERM` and untouched, a
/// thread of its own is allowed; with privilege any live task is.
pub fn schedattr_unprivileged_needs_same_process() -> Result<(), String> {
    scoped(USER, || {
        let stranger = task::spawn_fork().map_err(|error| format!("fork: {error}"))?;
        check!(
            credentials::of(stranger).uid == USER.uid,
            "the stranger is not the caller's user"
        );
        let weight = task::weight(stranger);
        for ask in [attr(OTHER, 5), attr(IDLE, 0), attr(BATCH, 19)] {
            let got = setattr(stranger as u64, &ask);
            check!(
                got == EPERM,
                "same user, other process: policy {} nice {}: {got:#x}",
                ask.policy,
                ask.nice
            );
        }
        check!(
            task::weight(stranger) == weight && getattr(stranger as u64)?.policy == OTHER,
            "a refused call changed the other process"
        );
        // Reading it is not a change.
        check!(
            sys(GETSCHEDULER, &[stranger as u64]) == 0,
            "getscheduler on another process"
        );
        // Its own process, from inside it: itself, and a thread of it.
        in_process(|process, thread| {
            check!(setattr(0, &attr(OTHER, 2)) == 0, "the process itself");
            check!(
                setattr(process as u64, &attr(OTHER, 3)) == 0,
                "itself by pid"
            );
            check!(setattr(thread as u64, &attr(OTHER, 4)) == 0, "its thread");
            check!(
                setattr(stranger as u64, &attr(OTHER, 4)) == EPERM,
                "a process that is not its own"
            );
            Ok(())
        })?;
        // A thread cannot reach out of its process either.
        let thread_of_stranger = {
            task::harness::switch_current(stranger);
            let thread = task::spawn_thread("schedattr", process::USER_STACK_TOP, 0, 0);
            task::harness::switch_current(task::KERNEL_TASK);
            thread.map_err(|error| format!("thread: {error}"))?
        };
        check!(
            setattr(thread_of_stranger as u64, &attr(OTHER, 6)) == EPERM,
            "a thread of the other process"
        );
        Ok(())
    })?;
    // Privilege reaches any task, whatever its process or user.
    scoped(ADMIN, || {
        let stranger = task::spawn_fork().map_err(|error| format!("fork: {error}"))?;
        credentials::set(stranger, Cred::new(2000, 2000, 0, 0, 0));
        check!(
            setattr(stranger as u64, &attr(IDLE, 9)) == 0,
            "admin, other user"
        );
        check!(
            getattr(stranger as u64)?.policy == IDLE
                && task::weight(stranger) == Some(task::MIN_WEIGHT),
            "the administrator's change did not land"
        );
        Ok(())
    })
}

/// `sched_setscheduler`, `sched_setparam`, `setpriority` and `getpriority` are
/// not offered (`ENOSYS`). WTF calls `sched_setattr` directly, but musl's
/// `pthread_setschedparam` calls `sched_setscheduler` and `pthread_setschedprio`
/// calls `sched_setparam`, so those two library calls fail until the syscalls
/// are wired in. Nothing can change a task's policy or nice value behind
/// `sched_getattr`'s back, and the two views agree by construction. Pinned here
/// so wiring one of them in has to route it through `schedattr`.
pub fn schedattr_other_setters_not_offered() -> Result<(), String> {
    scoped(USER, || {
        for (nr, name) in [
            (140, "getpriority"),
            (141, "setpriority"),
            (142, "sched_setparam"),
            (SETSCHEDULER, "sched_setscheduler"),
        ] {
            let got = sys(nr, &[0, 0, 0]);
            check!(got == ENOSYS, "{name} returned {got:#x}, not ENOSYS");
        }
        check!(getattr(0)?.policy == OTHER, "the probes changed the policy");
        Ok(())
    })
}

/// Tick the simulated clock one tick at a time until `done`, at most `limit`.
fn tick_until(limit: u64, done: impl Fn() -> bool) -> Result<(), String> {
    for _ in 0..limit {
        if done() {
            return Ok(());
        }
        task::harness::simulate_tick();
    }
    check!(done(), "the condition never came within {limit} ticks");
    Ok(())
}

/// A `SCHED_IDLE` (and a nice value) set while the CPU guard has demoted the
/// task is what the window's end restores: the live weight stays the
/// demotion's meanwhile, the stored policy reads back at once.
pub fn schedattr_applies_after_guard_demotion() -> Result<(), String> {
    scoped(ADMIN, || {
        task::set_blocked(true);
        task::guard::set_enabled(true);
        task::guard::reset_window();
        let outcome = (|| {
            let hog = task::spawn_fork().map_err(|error| format!("fork: {error}"))?;
            check!(task::set_priority(hog, PriorityClass::Interactive), "class");
            let peer = task::spawn_fork().map_err(|error| format!("fork: {error}"))?;
            check!(task::set_priority(peer, PriorityClass::Normal), "class");
            let demoted = || task::priority(hog) == Some(PriorityClass::Normal);
            let home = || task::priority(hog) == Some(PriorityClass::Interactive);
            let normal = PriorityClass::Normal.default_weight();

            for (policy, level) in [(IDLE, 0), (OTHER, -5)] {
                // Demoted: the hog ran past its class budget.
                tick_until(4 * WINDOW_TICKS, demoted)?;
                task::harness::switch_current(task::KERNEL_TASK);
                check!(
                    setattr(hog as u64, &attr(policy, level)) == 0,
                    "set policy {policy} nice {level} while demoted"
                );
                let read = getattr(hog as u64)?;
                check!(
                    read.policy == policy && read.nice == level,
                    "policy {} nice {} read back while demoted",
                    read.policy,
                    read.nice
                );
                check!(
                    demoted() && task::weight(hog) == Some(normal),
                    "the live weight {:?} moved while demoted (class {:?})",
                    task::weight(hog),
                    task::priority(hog)
                );
                // The window ends: home class, the assigned weight.
                tick_until(2 * WINDOW_TICKS, home)?;
                let want = weight_at(level, policy == IDLE);
                check!(
                    task::weight(hog) == Some(want),
                    "policy {policy} nice {level}: restored weight {:?}, want {want}",
                    task::weight(hog)
                );
                check!(
                    getattr(hog as u64)?.policy == policy,
                    "the policy was lost at the window's end"
                );
            }
            Ok(())
        })();
        for slot in 1..task::MAX_TASKS {
            if task::harness::state(slot).is_some() {
                task::harness::finish(slot, 0);
            }
        }
        task::harness::switch_current(task::KERNEL_TASK);
        while task::reap_child().is_some() {}
        task::set_blocked(false);
        task::guard::set_enabled(false);
        outcome
    })
}
