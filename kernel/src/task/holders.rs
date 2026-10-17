//! Who keeps a directory tree busy: the open files and working directories
//! under a path, by task. A removable volume is ejected only when this is
//! empty (`fs::fuse` `stop`), and the answer names the culprits.
//!
//! Only Linux-ABI tasks can hold anything: a native task's file calls are
//! stateless (by path), so it has no descriptor to leave behind.

use alloc::string::String;
use alloc::vec::Vec;

use super::{Fd, TASKS};

/// One task keeping something under a path busy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    /// The task's slot (its pid).
    pub pid: usize,
    /// The program's file name when it is known (`busybox`), else the
    /// task's own name.
    pub name: String,
    /// The open file's path, or the working directory itself.
    pub path: String,
    /// `path` is the task's working directory, not an open file.
    pub cwd: bool,
}

/// Most holders one scan collects: enough to name who to close, and a bound
/// on the work done with the task table locked.
pub const MAX_HOLDERS: usize = 64;

/// Whether `path` is `prefix` or lies below it, whole components only.
pub fn is_under(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The tasks with a file open or their working directory at or under
/// `prefix`, at most [`MAX_HOLDERS`] entries (one per task, path and kind
/// of hold; a descriptor duplicated or shared by threads may appear twice).
/// Stops scanning when the bound is reached.
pub fn under(prefix: &str) -> Vec<Holder> {
    let tasks = TASKS.lock();
    let mut found: Vec<Holder> = Vec::new();
    for (pid, slot) in tasks.iter().enumerate() {
        let Some(task) = slot else { continue };
        if matches!(task.state, super::TaskState::Done) {
            continue;
        }
        if found.len() >= MAX_HOLDERS {
            break;
        }
        // A syscall runs with interrupts off and this walks every
        // descriptor of every task: take pending interrupts between tasks
        // (the window runs no handler that needs a lock, so it is safe here).
        crate::arch::irq_window::poll_point();
        let program = task
            .linux
            .exe
            .as_deref()
            .and_then(|path| path.rsplit('/').next())
            .filter(|name| !name.is_empty())
            .map_or_else(|| String::from(task.name), String::from);
        let mut note = |path: String, cwd: bool| {
            if found.len() < MAX_HOLDERS {
                found.push(Holder {
                    pid,
                    name: program.clone(),
                    path,
                    cwd,
                });
            }
        };
        if let Some(cwd) = &task.cwd {
            if is_under(cwd, prefix) {
                note(String::from(&**cwd), true);
            }
        }
        for (_, fd) in task.fds.iter() {
            if let Fd::Vfs { file } = fd {
                let path = file.path();
                if is_under(&path, prefix) {
                    note(path, false);
                }
            }
        }
    }
    found
}
