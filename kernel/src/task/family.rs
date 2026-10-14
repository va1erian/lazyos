//! Who a child belongs to (issue #859): the **process** of the task that
//! forked it, not the thread. `Task::parent` names the forking thread's slot
//! (or, after that thread exits, a live sibling's); every question about
//! children (`wait4`, the child bell, `setpgid`) asks whether the child's
//! parent is in the asker's process. Also `PR_SET_PDEATHSIG`.

use super::process::reparent_children_locked;
use super::signal;
use super::{Task, TaskState, KERNEL_TASK, MAX_TASKS};

/// The thread-group leader of `slot`: its own slot for a process, the slot
/// `clone(CLONE_THREAD)` recorded for a thread.
pub(super) fn leader_of(tasks: &[Option<Task>; MAX_TASKS], slot: usize) -> usize {
    match tasks.get(slot).and_then(|task| task.as_ref()) {
        Some(task) if task.linux.tgid != 0 => task.linux.tgid,
        _ => slot,
    }
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

/// Children whose process asked to be signalled when its parent dies
/// (`PR_SET_PDEATHSIG`), flagged by [`finish_locked`] and signalled by
/// [`deliver_parent_death`] once the task table is unlocked.
static PENDING_PDEATH: super::slotmask::SlotMask = super::slotmask::SlotMask::new();

/// Send the parent-death signals [`finish_locked`] flagged. Call with the
/// task table unlocked, wherever a death is finished off.
pub(crate) fn deliver_parent_death() {
    for slot in PENDING_PDEATH.take().iter() {
        let sig = super::linuxstate::with_extras_of(slot, |extras| extras.pdeathsig).unwrap_or(0);
        if sig != 0 {
            let _ = signal::send_to_slot(KERNEL_TASK, slot, sig, signal::SigInfo::kernel());
        }
    }
}

/// `slot` died: its children go on with its process. While another thread of
/// the process lives, that thread inherits them (they belong to the process,
/// and `slot` may be reclaimed); when `slot` was the last, they are adopted by
/// init, and each that asked for a parent-death signal is flagged for it.
/// Returns how many went to init.
pub(super) fn release_children_locked(tasks: &mut [Option<Task>; MAX_TASKS], slot: usize) -> usize {
    let heir = (1..MAX_TASKS).find(|&other| {
        other != slot
            && tasks[other]
                .as_ref()
                .is_some_and(|task| task.state != TaskState::Done)
            && same_process(tasks, other, slot)
    });
    if let Some(heir) = heir {
        reparent_children_locked(tasks, slot, heir);
        return 0;
    }
    for (child, task) in tasks.iter().enumerate() {
        let Some(task) = task else { continue };
        if task.parent == slot && task.state != TaskState::Done && task.linux.pdeathsig != 0 {
            PENDING_PDEATH.set(child);
        }
    }
    reparent_children_locked(tasks, slot, KERNEL_TASK)
}
