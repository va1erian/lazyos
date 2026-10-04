//! Process tree, process groups and sessions (issue #59).
//!
//! Linux gives every process a `pid`, collects processes into process groups
//! (`pgid`) and groups into sessions (`sid`). LazyOS keeps the pid equal to the
//! scheduler slot: the Linux shim has always reported the slot as the pid, and
//! the ABI fixtures rely on that value being stable, so a separate pid space
//! would be churn without payoff until pid namespaces / credentials (#68).
//! Groups and sessions are therefore modelled *separately* from the pid:
//! [`Task::pgid`] and [`Task::sid`] hold the **pid of the leader** (which is
//! also its slot).
//!
//! The tree is derived, not linked: `MAX_TASKS` is 16, so scanning the task
//! table for `parent == slot` is cheaper than maintaining child lists on every
//! fork/reap and cannot go stale. When a task dies, every task whose parent is
//! that slot is re-parented to the kernel/init task (slot 0, pid 0), so
//! `ppid == 0` means init. This is Linux's "re-parent to init" rule with init
//! living in slot 0.
//!
//! The syscall semantics follow Linux:
//!
//! * a process may only change its own group or one of its children's
//!   (`setpgid`);
//! * a group must already exist in the target's session, or be formed by the
//!   target itself (`pgid == pid`);
//! * a session leader cannot be moved into another group, and crossing
//!   sessions is `EPERM`;
//! * `setsid` fails while the caller is a group leader (`pgid == pid`).
//!
//! Every death funnels through [`finish`] (or its lock-held variant
//! [`finish_locked`]), which is also where the signal layer (#60) hooks
//! `SIGCHLD` in: the parent is notified on its own wait queue *and* gets the
//! signal pending (a handler wakes it, a default-disposition parent is only
//! recorded).
//!
//! [`kill_group`] remains the blunt group-termination primitive; the signal
//! layer drives per-process termination instead.

use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use super::signal;
use super::wait::CHILD_EXIT;
use super::{Task, TaskState, KERNEL_TASK, MAX_TASKS, NEEDS_REDRAW, PENDING_CLOSE, TASKS};

/// Exit status recorded for a member terminated by [`kill_group`]: the
/// conventional `128 + SIGKILL(9)`. The signal layer terminates processes one
/// group at a time now; the group primitive stays for the test harness.
#[allow(dead_code)]
pub const KILLED_STATUS: u64 = 137;

/// Failures of the group/session syscalls, mapped to errno by `process::linux`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupError {
    /// No live process with that pid, or no such process group.
    NoSuchProcess,
    /// The move crosses sessions, targets a session leader, or names a process
    /// that is neither the caller nor one of its children.
    NotPermitted,
    /// The requested pgid is negative (Linux `EINVAL`).
    Invalid,
}

/// One row of [`process_list`]: the kernel-side view of a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessInfo {
    pub slot: usize,
    pub pid: usize,
    pub ppid: usize,
    pub pgid: usize,
    pub sid: usize,
    /// Placeholder until credentials land (#68): every task runs as uid 0.
    pub uid: u32,
    pub state: TaskState,
    pub name: &'static str,
}

/// The pid of a task slot. Pids are slots by design (see the module docs).
pub fn pid_of(slot: usize) -> usize {
    slot
}

/// The parent pid of a slot; 0 is the kernel/init task.
pub fn ppid_of(slot: usize) -> usize {
    TASKS
        .lock()
        .get(slot)
        .and_then(|task| task.as_ref())
        .map(|task| task.parent)
        .unwrap_or(0)
}

/// The process group id of a slot.
pub fn pgid_of(slot: usize) -> usize {
    TASKS
        .lock()
        .get(slot)
        .and_then(|task| task.as_ref())
        .map(|task| task.pgid)
        .unwrap_or(0)
}

/// The session id of a slot. Introspection API for tools/tests (#59).
#[allow(dead_code)]
pub fn sid_of(slot: usize) -> usize {
    TASKS
        .lock()
        .get(slot)
        .and_then(|task| task.as_ref())
        .map(|task| task.sid)
        .unwrap_or(0)
}

/// The session of process group `group` while the group has a live member
/// (every member of a group shares its session).
pub fn group_session(group: usize) -> Option<usize> {
    let tasks = TASKS.lock();
    tasks
        .iter()
        .flatten()
        .find(|task| task.pgid == group && task.state != TaskState::Done)
        .map(|task| task.sid)
}

/// Whether session `sid` still has a live member: a terminal stays a
/// session's controlling terminal only while the session exists.
pub fn session_alive(sid: usize) -> bool {
    sid != 0
        && TASKS
            .lock()
            .iter()
            .flatten()
            .any(|task| task.sid == sid && task.state != TaskState::Done)
}

/// The slot holding pid `pid`, if occupied. Introspection API for tools/tests.
#[allow(dead_code)]
pub fn find_by_pid(pid: usize) -> Option<usize> {
    let tasks = TASKS.lock();
    find_by_pid_locked(&tasks, pid)
}

/// The slots whose parent is `slot`: the process tree derived on demand.
/// Introspection API for tools/tests (#59).
#[allow(dead_code)]
pub fn children_of(slot: usize) -> Vec<usize> {
    let tasks = TASKS.lock();
    (1..MAX_TASKS)
        .filter(|&index| {
            tasks[index]
                .as_ref()
                .is_some_and(|task| task.parent == slot)
        })
        .collect()
}

/// Snapshot the task table as process rows, oldest slot first (the kernel task
/// included, as pid 0). Introspection API for tools/tests (#59).
#[allow(dead_code)]
pub fn process_list() -> Vec<ProcessInfo> {
    let tasks = TASKS.lock();
    tasks
        .iter()
        .enumerate()
        .filter_map(|(slot, task)| {
            task.as_ref().map(|task| ProcessInfo {
                slot,
                pid: pid_of(slot),
                ppid: task.parent,
                pgid: task.pgid,
                sid: task.sid,
                uid: 0,
                state: task.state,
                name: task.name,
            })
        })
        .collect()
}

/// Mark `slot` finished with `status` and re-parent its tasks to init.
///
/// Returns whether the task was running; a slot that is already `Done` keeps
/// its old status (a second exit is a no-op). This is the single place a task
/// dies, so the tree update is atomic with the state change: a child can never
/// observe a `Done` parent as its `ppid`.
pub(crate) fn finish(slot: usize, status: u64) -> bool {
    let parent = {
        let mut tasks = TASKS.lock();
        finish_locked(&mut tasks, slot, status)
    };
    let Some(parent) = parent else {
        return false;
    };
    after_finish(parent);
    true
}

/// The side effects of a [`finish_locked`] that returned `parent`, run once
/// the task table lock is dropped.
pub(crate) fn after_finish(parent: usize) {
    // The dead task's pipe ends close now, so a reader that has not reaped it
    // yet still sees end-of-file.
    super::close_exited_fds();
    NEEDS_REDRAW.store(true, Ordering::Relaxed);
    crate::dev::silence_exited();
    // The child-exit event is both a `SIGCHLD` and a wait-queue notification:
    // the signal is recorded/queued (and wakes a handler-armed parent), while
    // every `wait4` sleeper is woken to re-check for a reapable child. Both run
    // after dropping the task table, in queue-before-table order.
    signal::post_sigchld(parent);
    super::childbell::ring(parent);
    CHILD_EXIT.notify_all();
}

/// [`finish`] on a caller-held task table, returning the dead task's parent so
/// the caller can post `SIGCHLD` after releasing the lock. The timer signal
/// sweep (`task::signal::sweep`) needs this: it runs on the scheduler's lock
/// and cannot re-enter `finish`.
pub(crate) fn finish_locked(
    tasks: &mut [Option<Task>; MAX_TASKS],
    slot: usize,
    status: u64,
) -> Option<usize> {
    let parent = {
        let task = tasks[slot].as_mut()?;
        if task.state == TaskState::Done {
            return None;
        }
        task.state = TaskState::Done;
        task.wake_reason = None;
        task.exit_status = status;
        task.parent
    };
    // Stop the dead task's devices (interrupt line, DMA) and close its
    // descriptors before its parent can be slow to reap it; the actual work
    // runs later in task context, outside this lock.
    crate::dev::note_task_exited(slot);
    PENDING_CLOSE.set(slot);
    let reparented = reparent_children_locked(tasks, slot, KERNEL_TASK);
    if reparented > 0 {
        serial_println!("proc: task {slot} died; re-parented {reparented} task(s) to init");
    }
    Some(parent)
}

/// Terminate every task in process group `pgid`, returning how many were newly
/// marked `Done`.
///
/// The kernel/init task (slot 0) is exempt: init is not killable, matching the
/// rule that only init terminates itself. Signal delivery (#60) uses
/// `signal::terminate_process` instead; this blunt primitive remains for the
/// test harness and for a future group-wide `SIGKILL` fast path.
#[allow(dead_code)]
pub fn kill_group(pgid: usize) -> usize {
    let (killed, parents) = {
        let mut tasks = TASKS.lock();
        let members: Vec<usize> = (1..MAX_TASKS)
            .filter(|&slot| {
                tasks[slot]
                    .as_ref()
                    .is_some_and(|task| task.pgid == pgid && task.state != TaskState::Done)
            })
            .collect();
        let mut parents = Vec::new();
        for slot in members {
            // A killed task can no longer raise its children, so
            // `finish_locked` adopts them to init right away.
            if let Some(parent) = finish_locked(&mut tasks, slot, KILLED_STATUS) {
                parents.push(parent);
            }
        }
        (parents.len(), parents)
    };
    if killed > 0 {
        super::close_exited_fds();
        NEEDS_REDRAW.store(true, Ordering::Relaxed);
        // See `finish`: queue before task table, and the table is now unlocked.
        for parent in parents {
            signal::post_sigchld(parent);
            super::childbell::ring(parent);
        }
        CHILD_EXIT.notify_all();
    }
    killed
}

/// `setpgid(pid, pgid)` semantics (issue #59).
///
/// `caller` may change its own group (`pid == 0` or `pid == caller`) or one of
/// its children's. The target must be in the caller's session, must not be a
/// session leader, and `pgid` (0 means "the target's pid", forming a new group)
/// must name an existing process in that session. Zombies cannot be re-grouped.
pub(crate) fn setpgid(caller: usize, pid: i64, pgid: i64) -> Result<(), GroupError> {
    if pgid < 0 {
        return Err(GroupError::Invalid);
    }
    let mut tasks = TASKS.lock();
    let target = resolve_pid_locked(&tasks, caller, pid)?;
    let (target_pid, target_sid, target_pgid, target_parent, target_done, caller_sid) = {
        let Some(target_task) = tasks[target].as_ref() else {
            return Err(GroupError::NoSuchProcess);
        };
        let Some(caller_task) = tasks[caller].as_ref() else {
            return Err(GroupError::NoSuchProcess);
        };
        (
            pid_of(target),
            target_task.sid,
            target_task.pgid,
            target_task.parent,
            target_task.state == TaskState::Done,
            caller_task.sid,
        )
    };
    if target_done {
        return Err(GroupError::NoSuchProcess);
    }
    if target != caller && target_parent != caller {
        return Err(GroupError::NotPermitted);
    }
    if caller_sid != target_sid {
        return Err(GroupError::NotPermitted);
    }
    if target_sid == target_pid {
        return Err(GroupError::NotPermitted);
    }
    let new_pgid = if pgid == 0 { target_pid } else { pgid as usize };
    if new_pgid == target_pgid {
        return Ok(()); // already a member: `setpgid` is idempotent
    }
    if new_pgid != target_pid
        && !tasks
            .iter()
            .flatten()
            .any(|task| task.sid == target_sid && task.pgid == new_pgid)
    {
        return Err(GroupError::NoSuchProcess);
    }
    if let Some(task) = tasks[target].as_mut() {
        task.pgid = new_pgid;
    }
    Ok(())
}

/// `setsid()`: make `caller` a session and group leader, returning the new sid.
///
/// Fails with [`GroupError::NotPermitted`] while the caller is already a group
/// leader (`pgid == pid`), exactly like Linux's `EPERM`.
pub(crate) fn setsid(caller: usize) -> Result<usize, GroupError> {
    let mut tasks = TASKS.lock();
    let pid = pid_of(caller);
    let Some(task) = tasks[caller].as_ref() else {
        return Err(GroupError::NoSuchProcess);
    };
    if task.pgid == pid {
        return Err(GroupError::NotPermitted);
    }
    if let Some(task) = tasks[caller].as_mut() {
        task.pgid = pid;
        task.sid = pid;
    }
    Ok(pid)
}

/// `getpgid(pid)`: the group of `pid` (0 = the caller). Unlike `getsid`,
/// Linux does not require the target to share the caller's session.
pub(crate) fn getpgid(caller: usize, pid: i64) -> Result<usize, GroupError> {
    let tasks = TASKS.lock();
    let target = resolve_pid_locked(&tasks, caller, pid)?;
    Ok(tasks[target].as_ref().map(|task| task.pgid).unwrap_or(0))
}

/// `getsid(pid)`: the session of `pid` (0 = the caller). Querying a process in
/// another session is `EPERM`, as in Linux.
pub(crate) fn getsid(caller: usize, pid: i64) -> Result<usize, GroupError> {
    let tasks = TASKS.lock();
    let target = resolve_pid_locked(&tasks, caller, pid)?;
    let caller_sid = tasks[caller]
        .as_ref()
        .map(|task| task.sid)
        .ok_or(GroupError::NoSuchProcess)?;
    let target_sid = tasks[target]
        .as_ref()
        .map(|task| task.sid)
        .ok_or(GroupError::NoSuchProcess)?;
    if target != caller && target_sid != caller_sid {
        return Err(GroupError::NotPermitted);
    }
    Ok(target_sid)
}

/// Move every task whose parent is `slot` to `new_parent` (an adoption).
///
/// Callers hold the task table lock: adoption must commit in the same critical
/// section as the death that caused it.
fn reparent_children_locked(
    tasks: &mut [Option<Task>; MAX_TASKS],
    slot: usize,
    new_parent: usize,
) -> usize {
    if slot == new_parent {
        return 0;
    }
    let mut moved = 0;
    for task in tasks.iter_mut().flatten() {
        if task.parent == slot {
            task.parent = new_parent;
            moved += 1;
        }
    }
    moved
}

/// Resolve the pid argument of the group syscalls against the table: 0 means
/// "the caller"; anything else must name an occupied slot.
fn resolve_pid_locked(
    tasks: &[Option<Task>; MAX_TASKS],
    caller: usize,
    pid: i64,
) -> Result<usize, GroupError> {
    if pid == 0 {
        return if caller < MAX_TASKS && tasks[caller].is_some() {
            Ok(caller)
        } else {
            Err(GroupError::NoSuchProcess)
        };
    }
    if pid < 0 {
        return Err(GroupError::NoSuchProcess);
    }
    find_by_pid_locked(tasks, pid as usize).ok_or(GroupError::NoSuchProcess)
}

fn find_by_pid_locked(tasks: &[Option<Task>; MAX_TASKS], pid: usize) -> Option<usize> {
    (pid < MAX_TASKS && tasks[pid].is_some()).then_some(pid)
}
