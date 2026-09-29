//! The per-task working directory of the Linux ABI.
//!
//! The kernel only *stores* the directory; what a relative path means, and
//! whether a `chdir` target is acceptable, is decided by the syscall layer
//! (`process::linux::cwd`), which is the one place that resolves paths.
//!
//! The value lives in [`Task::cwd`] like every other per-task attribute, so
//! its lifecycle needs no code of its own: `fork` and threads clone the
//! reference, `execve` replaces the address space of the same task (the
//! directory survives, as POSIX requires) and a task's teardown drops it.

use alloc::string::String;
use alloc::sync::Arc;

use super::*;

/// The directory a task is in until it changes it.
const ROOT: &str = "/";

/// The calling task's working directory: an absolute path with `.`/`..`
/// already folded. `/` until the task first changes directory.
pub fn cwd() -> String {
    // Only the reference is cloned under the lock; the string is built after
    // it, so the heap is never entered while holding the task table.
    let held = TASKS.lock()[current()]
        .as_ref()
        .and_then(|task| task.cwd.clone());
    String::from(held.as_deref().unwrap_or(ROOT))
}

/// Make `path` the calling task's working directory. The caller has already
/// resolved and validated it (absolute, normalized, an existing directory).
pub fn set_cwd(path: &str) {
    // Allocated before the lock is taken, and the previous value is dropped
    // after it is released: freeing it enters the heap, which the interrupted
    // task may hold (see `PENDING_RECLAIM`).
    let new: Arc<str> = Arc::from(path);
    let old = TASKS.lock()[current()]
        .as_mut()
        .and_then(|task| task.cwd.replace(new));
    drop(old);
}
