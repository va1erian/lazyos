//! Per-address-space heap break, `brk` and `mmap` cursors.

use super::*;

/// The current task's heap break (for `sbrk`).
pub fn heap_break() -> u64 {
    let tasks = TASKS.lock();
    tasks[current()].as_ref().map(|t| t.heap_break).unwrap_or(0)
}

/// Set the current task's heap break.
pub fn set_heap_break(value: u64) {
    let mut tasks = TASKS.lock();
    if let Some(task) = tasks[current()].as_mut() {
        task.heap_break = value;
    }
}

/// Set the current task's address space (used by `execve`).
///
/// When no other task references the previous table it is torn down here,
/// which closes the fork+exec leak. `execve` switches `CR3` before calling
/// this, so the old table is inactive. Tables still shared with
/// `clone(CLONE_VM)` threads are left alone; those threads currently have no
/// teardown path of their own.
pub fn set_pml4(value: u64) {
    let mut tasks = TASKS.lock();
    let me = current();
    let old = tasks[me].as_ref().map(|task| task.pml4);
    if let Some(task) = tasks[me].as_mut() {
        task.pml4 = value;
    }
    let orphaned = old.filter(|old| {
        *old != value
            && !tasks.iter().enumerate().any(|(other, task)| {
                other != me && task.as_ref().is_some_and(|task| task.pml4 == *old)
            })
    });
    // Another *live* thread still in the old address space (an `execve` from
    // one thread of several; its siblings are not stopped yet): the process's
    // timers stay where they are for them.
    let shared_live = old.is_some_and(|old| {
        tasks.iter().enumerate().any(|(other, task)| {
            other != me
                && task
                    .as_ref()
                    .is_some_and(|task| task.pml4 == old && task.state != TaskState::Done)
        })
    });
    // A thread that execs becomes its own thread-group leader, in the new
    // address space (there is no de_thread: its siblings stay behind). If it
    // was the last live task of the old process and the leader is a zombie,
    // the leader is reapable now: tell its parent.
    let mut notify = 0;
    if let Some(old) = old.filter(|old| *old != value) {
        let leader = tasks[me].as_ref().map_or(0, |task| task.linux.tgid);
        if let Some(task) = tasks[me].as_mut() {
            task.linux.tgid = 0;
        }
        if leader != 0 && !shared_live {
            notify = tasks[leader]
                .as_ref()
                .filter(|task| task.state == TaskState::Done && task.pml4 == old)
                .map_or(0, |task| task.parent);
        }
    }
    drop(tasks);
    if notify != 0 {
        super::process::after_finish(notify);
    }
    // A kill that reached the task while it loaded the new image must still
    // end it: the old table's signal state is about to be dropped.
    if let Some(old) = old.filter(|old| *old != value) {
        signal::carry_kill(old, value);
        // The thread keeps its blocked mask and pending signals across the
        // exec (as on Linux), but not its alternate stack.
        signal::exec_thread(me);
        // Interval timers survive an exec (the process is the same); POSIX
        // timers do not. Only when no sibling still lives in the old space.
        if !shared_live {
            super::itimer::exec(old, value);
        }
    }
    if let Some(old) = orphaned {
        // Guard against freeing whatever `CR3` currently points at (only
        // possible if `execve` were preempted between its switch and here).
        if mem::kernel_table().as_u64() == old {
            serial_println!("mem: not freeing active page table {old:#x}");
            return;
        }
        forget_bumps(old);
        // `execve` starts a fresh signal disposition table (Linux keeps SIG_IGN
        // but resets handlers; a fresh table means ignored ones reset too).
        signal::forget(old);
        // The old image's shared file pages go to their files now, in the
        // exec'ing task's own context.
        mem::filemap::flush_table(PhysAddr::new(old));
        let released = mem::free_user_table(PhysAddr::new(old));
        let stats = mem::frame_stats();
        serial_println!(
            "mem: execve released {released} frames, {} free of {}",
            stats.free,
            stats.total
        );
    }
}

/// How many tasks run in the address space `pml4`.
pub fn address_space_users(pml4: u64) -> usize {
    TASKS
        .lock()
        .iter()
        .flatten()
        .filter(|task| task.pml4 == pml4 && task.state != TaskState::Done)
        .count()
}

/// The current task's `brk` break.
pub fn brk() -> u64 {
    with_bump(|bump| bump.brk).unwrap_or(0)
}

/// Where the current address space's break started (its floor).
pub fn brk_start() -> u64 {
    with_bump(|bump| bump.brk_start).unwrap_or(0)
}

/// Set the current task's Linux `brk` break.
pub fn set_brk(value: u64) {
    let _ = with_bump(|bump| bump.brk = value);
}

/// The current address space's anonymous `mmap` bump pointer.
pub fn mmap_next() -> u64 {
    with_bump(|bump| bump.mmap_next).unwrap_or(0)
}

/// Set the current address space's anonymous `mmap` bump pointer.
pub fn set_mmap_next(value: u64) {
    let _ = with_bump(|bump| bump.mmap_next = value);
}
