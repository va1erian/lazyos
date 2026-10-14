//! Per-task Linux-ABI state that the scheduler never looks at: which tasks
//! share a descriptor table or a filesystem context (`CLONE_FILES`,
//! `CLONE_FS`), the thread-group id `getpid` reports, the signal that ended
//! the task (so `wait4` can report `WIFSIGNALED`), and the path of the program
//! the task runs (`/proc/self/exe`).
//!
//! Sharing is modelled as *share groups*: tasks with the same non-zero
//! `files_group` keep identical descriptor tables, because every change to one
//! table is mirrored into the others under the task-table lock
//! ([`super::fdshare`]). The table storage itself is unchanged (one array per
//! task); a group is only a promise that the arrays stay equal. `0` means
//! "private", so a fresh task shares nothing by construction.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, Ordering};

use super::*;

/// The Linux-only per-task fields (see the module docs).
#[derive(Clone, Default)]
pub struct LinuxExtras {
    /// Descriptor-table share group (`CLONE_FILES`); 0 = private.
    pub files_group: u32,
    /// Working-directory share group (`CLONE_FS`); 0 = private.
    pub fs_group: u32,
    /// The signal that terminated the task, 0 when it exited normally.
    pub term_signal: u8,
    /// The thread-group leader's slot for a thread; 0 for a process (its own
    /// slot is its pid).
    pub tgid: usize,
    /// The program image's path, as `execve` resolved it.
    pub exe: Option<Arc<str>>,
    /// `set_robust_list` head (0 = none): walked when the thread exits.
    pub robust_head: u64,
    /// `prctl(PR_SET_NAME)` name; empty means the task's own name.
    pub comm: [u8; 16],
    /// `prctl(PR_SET_NO_NEW_PRIVS)`: sticky, inherited by every child.
    pub no_new_privs: bool,
    /// The line discipline of this task's console window (root tasks only;
    /// created on first use, see [`super::consoletty`]).
    pub console: Option<alloc::boxed::Box<crate::tty::Ldisc>>,
    /// The controlling pseudo-terminal (`TIOCSCTTY`, or opening a slave),
    /// which `/dev/tty` opens; `None` means the console window.
    pub ctty: Option<Arc<crate::tty::pty::Pty>>,
    /// What `sched_setattr` last set (read back by `sched_getattr`).
    pub sched: SchedState,
    /// Thread-directed signals (`tkill`/`tgkill` with a real signal) the
    /// process has sent. Kept on the thread-group leader only and read as
    /// `LazyOS_ThreadSignals` in `/proc/self/status`: a guard that the
    /// per-thread signal path is used (JavaScriptCore's collector suspends a
    /// parked mutator with one).
    pub thread_signals: u64,
}

/// A thread's Linux scheduling attributes. Only the plain policies are ever
/// stored (`SCHED_OTHER`, `SCHED_BATCH`, `SCHED_IDLE`); the nice value also
/// sets the task's stride weight ([`super::nice`]), the policy only `IDLE`.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct SchedState {
    /// `SCHED_OTHER` (0), `SCHED_BATCH` (3) or `SCHED_IDLE` (5).
    pub policy: u8,
    /// `SCHED_FLAG_RESET_ON_FORK`: a child starts with a nice value of 0 at
    /// most, and without the flag itself.
    pub reset_on_fork: bool,
    /// -20..=19.
    pub nice: i8,
}

impl SchedState {
    /// The `SCHED_IDLE` policy number.
    pub const IDLE: u8 = 5;

    /// Whether the task runs at the least weight (`SCHED_IDLE`).
    pub fn is_idle(self) -> bool {
        self.policy == Self::IDLE
    }

    /// What a forked or cloned child starts with: the same, but a negative
    /// nice value is lost to `reset_on_fork`, and the flag is not inherited.
    pub fn for_child(self) -> SchedState {
        SchedState {
            policy: self.policy,
            reset_on_fork: false,
            nice: if self.reset_on_fork {
                self.nice.max(0)
            } else {
                self.nice
            },
        }
    }

    /// The weight a child starts with: its parent's (`weight`), or the
    /// class default when `reset_on_fork` took away a negative nice value.
    pub fn child_weight(self, class: PriorityClass, weight: u16) -> u16 {
        if self.reset_on_fork && self.nice < 0 {
            class.default_weight()
        } else {
            weight
        }
    }
}

/// Source of fresh share-group ids (never 0).
static NEXT_GROUP: AtomicU32 = AtomicU32::new(1);

fn new_group() -> u32 {
    // Wrapping past u32::MAX would take four billion clones; skip 0 anyway.
    let id = NEXT_GROUP.fetch_add(1, Ordering::Relaxed);
    if id == 0 {
        NEXT_GROUP.fetch_add(1, Ordering::Relaxed)
    } else {
        id
    }
}

/// What a new thread shares with its creator (`clone` flags).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct ThreadShare {
    pub files: bool,
    pub fs: bool,
}

/// The extras of a thread created by `parent` (slot `parent_slot`): the same
/// thread group, the same program, and the share groups `share` asks for (a
/// group is created on first use, and the creator joins it too).
pub(super) fn for_thread(parent: &mut Task, parent_slot: usize, share: ThreadShare) -> LinuxExtras {
    if share.files && parent.linux.files_group == 0 {
        parent.linux.files_group = new_group();
    }
    if share.fs && parent.linux.fs_group == 0 {
        parent.linux.fs_group = new_group();
    }
    let leader = match parent.linux.tgid {
        0 => parent_slot,
        tgid => tgid,
    };
    LinuxExtras {
        files_group: if share.files {
            parent.linux.files_group
        } else {
            0
        },
        fs_group: if share.fs { parent.linux.fs_group } else { 0 },
        term_signal: 0,
        tgid: leader,
        exe: parent.linux.exe.clone(),
        robust_head: 0,
        comm: parent.linux.comm,
        no_new_privs: parent.linux.no_new_privs,
        console: None,
        ctty: parent.linux.ctty.clone(),
        sched: parent.linux.sched.for_child(),
        thread_signals: 0,
    }
}

/// The extras of a forked process: nothing shared, the same program.
pub(super) fn for_fork(parent: &Task) -> LinuxExtras {
    LinuxExtras {
        exe: parent.linux.exe.clone(),
        comm: parent.linux.comm,
        no_new_privs: parent.linux.no_new_privs,
        ctty: parent.linux.ctty.clone(),
        sched: parent.linux.sched.for_child(),
        ..LinuxExtras::default()
    }
}

/// `getpid`: the thread-group leader's slot (a process's own slot).
pub fn tgid() -> usize {
    let me = current();
    match TASKS.lock()[me].as_ref().map(|task| task.linux.tgid) {
        Some(0) | None => me,
        Some(leader) => leader,
    }
}

/// Count one thread-directed signal sent by the calling task's process. It
/// lands on the thread-group leader; if the leader's slot is already gone the
/// count is lost, which only a diagnostic can notice.
pub fn note_thread_signal() {
    with_extras_of(tgid(), |extras| {
        extras.thread_signals = extras.thread_signals.saturating_add(1);
    });
}

/// How many thread-directed signals the calling task's process has sent.
pub fn thread_signals() -> u64 {
    with_extras_of(tgid(), |extras| extras.thread_signals).unwrap_or(0)
}

/// The signal that ended `slot` (0 for a normal exit; tests and diagnostics).
#[allow(dead_code)]
pub fn term_signal(slot: usize) -> u8 {
    TASKS
        .lock()
        .get(slot)
        .and_then(|task| task.as_ref())
        .map_or(0, |task| task.linux.term_signal)
}

/// Record that `slot` is being ended by signal `sig` (before it finishes).
pub(crate) fn note_term_signal(tasks: &mut [Option<Task>; MAX_TASKS], slot: usize, sig: u8) {
    if let Some(task) = tasks[slot].as_mut() {
        if task.state != TaskState::Done {
            task.linux.term_signal = sig;
        }
    }
}

/// Whether `slot` holds a task that has not finished, in address space
/// `space` (a recycled slot or a reused table is a different process).
pub fn running_in(slot: usize, space: u64) -> bool {
    TASKS
        .lock()
        .get(slot)
        .and_then(|task| task.as_ref())
        .is_some_and(|task| task.pml4 == space && task.state != TaskState::Done)
}

/// The calling task's controlling pseudo-terminal, if any.
pub fn ctty() -> Option<Arc<crate::tty::pty::Pty>> {
    TASKS.lock()[current()]
        .as_ref()
        .and_then(|task| task.linux.ctty.clone())
}

/// Set (or clear) the calling task's controlling pseudo-terminal.
pub fn set_ctty(pty: Option<Arc<crate::tty::pty::Pty>>) {
    let old = TASKS.lock()[current()]
        .as_mut()
        .map(|task| core::mem::replace(&mut task.linux.ctty, pty));
    drop(old);
}

/// The calling task's program path, if `execve` recorded one.
pub fn exe() -> Option<Arc<str>> {
    TASKS.lock()[current()]
        .as_ref()
        .and_then(|task| task.linux.exe.clone())
}

/// Record the program a native task was started from (`spawnv`), which
/// `slot` is not yet running: unlike [`set_exe`] it leaves the share groups.
pub fn record_program(slot: usize, path: &str) {
    let new: Arc<str> = Arc::from(path);
    let comm = comm_of_path(path);
    let old = TASKS.lock()[slot].as_mut().and_then(|task| {
        task.linux.comm = comm;
        task.linux.exe.replace(new)
    });
    drop(old);
}

/// The program task `slot` was started from, if one was recorded.
pub fn program_of(slot: usize) -> Option<Arc<str>> {
    TASKS
        .lock()
        .get(slot)
        .and_then(|task| task.as_ref())
        .and_then(|task| task.linux.exe.clone())
}

/// Record the calling task's new program (`execve`), and leave any
/// descriptor-table share group: like Linux, an image that replaces a thread's
/// program stops sharing its table with the old threads.
pub fn set_exe(path: &str) {
    let new: Arc<str> = Arc::from(path);
    let old = TASKS.lock()[current()].as_mut().and_then(|task| {
        task.linux.files_group = 0;
        task.linux.fs_group = 0;
        // The name is the new image's, as on Linux (`PR_SET_NAME` is lost).
        task.linux.comm = comm_of_path(path);
        task.linux.exe.replace(new)
    });
    drop(old);
}

/// The calling task's share groups `(files, fs)` (tests and diagnostics).
#[allow(dead_code)]
pub fn share_groups(slot: usize) -> (u32, u32) {
    TASKS
        .lock()
        .get(slot)
        .and_then(|task| task.as_ref())
        .map_or((0, 0), |task| (task.linux.files_group, task.linux.fs_group))
}

/// Run `f` on the calling task's extras (`None` if the slot is empty).
pub fn with_extras<R>(f: impl FnOnce(&mut LinuxExtras) -> R) -> Option<R> {
    TASKS.lock()[current()]
        .as_mut()
        .map(|task| f(&mut task.linux))
}

/// Run `f` on slot `slot`'s extras (`None` if the slot is empty).
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn with_extras_of<R>(slot: usize, f: impl FnOnce(&mut LinuxExtras) -> R) -> Option<R> {
    TASKS
        .lock()
        .get_mut(slot)?
        .as_mut()
        .map(|task| f(&mut task.linux))
}

/// The 15-byte name (`comm`) of the program at `path`: its last component.
pub fn comm_of_path(path: &str) -> [u8; 16] {
    let base = path.rsplit('/').next().unwrap_or(path).as_bytes();
    let mut name = [0u8; 16];
    let len = base.len().min(15);
    name[..len].copy_from_slice(&base[..len]);
    name
}

/// The task's display name: the `PR_SET_NAME` name, else its own.
pub fn comm(slot: usize) -> ([u8; 16], usize) {
    let tasks = TASKS.lock();
    let Some(task) = tasks.get(slot).and_then(|task| task.as_ref()) else {
        return ([0; 16], 0);
    };
    comm_of(task)
}

/// [`comm`] of a task the caller already has: the name `execve` or
/// `PR_SET_NAME` gave it, else the kernel's own (`fork`, `thread`, a service).
pub fn comm_of(task: &Task) -> ([u8; 16], usize) {
    let len = task.linux.comm.iter().position(|&b| b == 0).unwrap_or(16);
    if len > 0 {
        return (task.linux.comm, len);
    }
    let mut name = [0u8; 16];
    let bytes = task.name.as_bytes();
    let len = bytes.len().min(15);
    name[..len].copy_from_slice(&bytes[..len]);
    (name, len)
}
