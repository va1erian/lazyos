//! Who a child belongs to (issue #859): the **process** of the task that
//! forked it, not the thread. `Task::parent` names the forking thread's slot
//! (or, after that thread exits, a live sibling's); every question about
//! children (`wait4`, the child bell, `setpgid`) asks whether the child's
//! parent is in the asker's process. Also `PR_SET_PDEATHSIG`.

#[cfg(lazyos_tests)]
use core::sync::atomic::AtomicU32;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::process::reparent_children_locked;
use super::signal;
use super::{Task, TaskState, KERNEL_TASK, MAX_TASKS};

/// The thread-group leader of `slot`: its own slot for a process, the slot
/// `clone(CLONE_THREAD)` recorded for a thread. A leader stays a zombie until
/// its last thread is gone (`reapable`), so the recorded slot cannot have been
/// reused while the thread runs; the check keeps a stale value from naming
/// somebody else's process anyway.
pub(super) fn leader_of(tasks: &[Option<Task>; MAX_TASKS], slot: usize) -> usize {
    match tasks.get(slot).and_then(|task| task.as_ref()) {
        // The recorded leader must still live in the thread's own address
        // space (kernel threads of the test suite share the kernel's).
        Some(task)
            if task.linux.tgid != 0
                && tasks
                    .get(task.linux.tgid)
                    .and_then(|leader| leader.as_ref())
                    .is_some_and(|leader| leader.pml4 == task.pml4) =>
        {
            task.linux.tgid
        }
        _ => slot,
    }
}

/// `getppid` of `slot`: the leader of the process that forked its process
/// (0: init).
pub(super) fn ppid(tasks: &[Option<Task>; MAX_TASKS], slot: usize) -> usize {
    let parent = tasks
        .get(leader_of(tasks, slot))
        .and_then(|task| task.as_ref())
        .map_or(0, |task| task.parent);
    leader_of(tasks, parent)
}

/// Whether the finished task `task` (in `slot`) may be reaped or reclaimed:
/// a thread-group leader stays a zombie while a thread of its process runs
/// (Linux's `delay_group_leader`), so its slot, the process's identity, is not
/// reused under the threads.
pub(super) fn reapable(tasks: &[Option<Task>; MAX_TASKS], slot: usize, task: &Task) -> bool {
    task.linux.tgid != 0
        || !tasks
            .iter()
            .enumerate()
            .any(|(other, t)| other != slot && is_live(t) && same_process(tasks, other, slot))
}

/// Whom the death of `slot` (already `Done`) tells: the parent whose wait is
/// now answerable, or `None` for nobody. A process is reapable only when its
/// leader is finished *and* no thread of it runs, so a leader that dies
/// first notifies nobody (the parent could not reap it), and the last thread
/// to go tells the leader's parent, with `SIGCHLD`, the child bell and the
/// `wait4` queue. `parent` is the dead task's own parent.
pub(super) fn notify_target(
    tasks: &[Option<Task>; MAX_TASKS],
    slot: usize,
    parent: usize,
) -> Option<usize> {
    if (1..MAX_TASKS)
        .any(|other| other != slot && is_live(&tasks[other]) && same_process(tasks, other, slot))
    {
        return None;
    }
    match leader_of(tasks, slot) {
        leader if leader == slot => Some(parent),
        leader => tasks[leader].as_ref().map(|task| task.parent),
    }
}

/// Test builds count the parents told of a death, per parent slot, so a test
/// can assert that a process's parent hears of it exactly once.
#[cfg(lazyos_tests)]
static NOTIFIED: [AtomicU32; MAX_TASKS] = [const { AtomicU32::new(0) }; MAX_TASKS];

/// A death told `parent` (its `SIGCHLD`, bell and `wait4` queue are next).
pub(crate) fn count_notification(parent: usize) {
    #[cfg(lazyos_tests)]
    if let Some(count) = NOTIFIED.get(parent) {
        count.fetch_add(1, Ordering::Relaxed);
    }
    #[cfg(not(lazyos_tests))]
    let _ = parent;
}

/// Test hook: how many times `parent` was told since [`reset_notifications`].
#[cfg(lazyos_tests)]
pub fn notifications(parent: usize) -> u32 {
    NOTIFIED[parent].load(Ordering::Relaxed)
}

/// Test hook: zero the counters.
#[cfg(lazyos_tests)]
pub fn reset_notifications() {
    for count in &NOTIFIED {
        count.store(0, Ordering::Relaxed);
    }
}

/// A group exit began in process `pml4` with `status`: a leader that left
/// earlier reports the group's status (and terminating signal) when it is
/// reaped, as Linux's `wait_task_zombie` does.
pub(super) fn note_group_exit(
    tasks: &mut [Option<Task>; MAX_TASKS],
    pml4: u64,
    status: u64,
    signaled: bool,
) {
    for task in tasks.iter_mut().flatten() {
        if task.pml4 == pml4 && task.state == TaskState::Done && task.linux.tgid == 0 {
            task.exit_status = status;
            task.linux.term_signal = if signaled && (129..=128 + 64).contains(&status) {
                (status - 128) as u8
            } else {
                0
            };
        }
    }
}

fn is_live(task: &Option<Task>) -> bool {
    task.as_ref()
        .is_some_and(|task| task.state != TaskState::Done)
}

/// Whether slots `a` and `b` are threads of one process: the same task, or
/// tasks that share a user address space (the key every per-process table
/// uses). Kernel tasks all share the kernel's table without being threads.
pub(super) fn same_process(tasks: &[Option<Task>; MAX_TASKS], a: usize, b: usize) -> bool {
    if a == b {
        return true;
    }
    let (Some(x), Some(y)) = (
        tasks.get(a).and_then(|task| task.as_ref()),
        tasks.get(b).and_then(|task| task.as_ref()),
    ) else {
        return false;
    };
    x.pml4 == y.pml4
        && tasks[KERNEL_TASK]
            .as_ref()
            .is_none_or(|kernel| kernel.pml4 != x.pml4)
}

/// Whether `child` was forked by the process that `me` is a thread of. A
/// child belongs to its parent's **process**, not to the thread that called
/// `fork`: any thread may `wait4` for it (WebKit forks on one thread and reaps
/// on another), and it stays with the process when the forking thread exits.
pub(super) fn is_child_of(tasks: &[Option<Task>; MAX_TASKS], me: usize, child: &Task) -> bool {
    child.parent == me || (child.parent != 0 && same_process(tasks, child.parent, me))
}

/// Live threads (and the address space they were flagged in) whose process
/// asked to be signalled when its parent dies (`PR_SET_PDEATHSIG`), flagged
/// by [`release_children_locked`] and signalled by [`deliver_parent_death`]
/// once the task table is unlocked. A flagged slot whose task has since been
/// replaced by another process is not signalled.
static PENDING_PDEATH: super::slotmask::SlotMask = super::slotmask::SlotMask::new();
static PENDING_SPACE: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];
/// Set while some call to [`deliver_parent_death`] is draining the flags.
static DELIVERING: AtomicBool = AtomicBool::new(false);

/// Send the parent-death signals [`release_children_locked`] flagged. Call
/// with the task table unlocked, wherever a death is finished off.
///
/// Iterative and not re-entrant: a signal that kills its target (SIGKILL)
/// finishes it, which flags *its* children and calls back in here. That
/// nested call returns at once and the outermost call picks the new flags up,
/// so a chain of N processes each asking for SIGKILL costs a loop, not N
/// levels of kernel stack.
pub(crate) fn deliver_parent_death() {
    if DELIVERING.swap(true, Ordering::Acquire) {
        return;
    }
    loop {
        for slot in PENDING_PDEATH.take().iter() {
            let space = PENDING_SPACE[slot].load(Ordering::Acquire);
            let sig = super::linuxstate::pdeathsig_in(slot, space);
            if sig != 0 {
                let _ = signal::send_to_slot(KERNEL_TASK, slot, sig, signal::SigInfo::kernel());
            }
        }
        DELIVERING.store(false, Ordering::Release);
        // A flag set while the last batch ran (by a nested call that returned
        // at once, or an interrupt) is this call's to deliver.
        if !PENDING_PDEATH.any() || DELIVERING.swap(true, Ordering::Acquire) {
            return;
        }
    }
}

/// `slot` died: its children go on with its process. While another thread of
/// the process lives, that thread inherits them (they belong to the process,
/// and `slot` may be reclaimed); when `slot` was the last, they are adopted by
/// init, and each child process that asked for a parent-death signal (on any
/// of its live threads) is flagged for it. Returns how many went to init.
pub(super) fn release_children_locked(tasks: &mut [Option<Task>; MAX_TASKS], slot: usize) -> usize {
    let heir = (1..MAX_TASKS)
        .find(|&other| other != slot && is_live(&tasks[other]) && same_process(tasks, other, slot));
    if let Some(heir) = heir {
        reparent_children_locked(tasks, slot, heir);
        return 0;
    }
    for child in tasks.iter().flatten().filter(|task| task.parent == slot) {
        // The child's process: any live thread of it that carries the signal
        // (the leader may be a zombie while a thread runs).
        let carrier = tasks.iter().enumerate().find(|(_, other)| {
            other.as_ref().is_some_and(|other| {
                other.state != TaskState::Done
                    && other.pml4 == child.pml4
                    && other.linux.pdeathsig != 0
            })
        });
        if let Some((carrier, Some(other))) = carrier {
            PENDING_SPACE[carrier].store(other.pml4, Ordering::Release);
            PENDING_PDEATH.set(carrier);
        }
    }
    reparent_children_locked(tasks, slot, KERNEL_TASK)
}
