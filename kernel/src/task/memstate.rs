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

/// Set the current task's address space (used by `execve`, after
/// [`dethread`](super::dethread) ended the other threads).
///
/// When no other task references the previous table it is torn down here,
/// which closes the fork+exec leak. `execve` switches `CR3` before calling
/// this, so the old table is inactive. A finished sibling not yet reclaimed
/// still names the old table; its reclamation frees it instead.
///
/// The process keeps its pid. A thread that execs while its leader is a
/// zombie (killed by the de-thread, or gone earlier) takes the leader along
/// into the new address space: the zombie keeps the pid, parent, group and
/// session, the thread keeps naming it as its leader (`getpid`), and the
/// parent hears of the process only when the new image ends. Unlike Linux
/// the thread's own tid stays what it was (a tid is a scheduler slot).
pub fn set_pml4(value: u64) {
    let me = current();
    let (old, shared_live) = {
        let tasks = TASKS.lock();
        let old = tasks[me].as_ref().map(|task| task.pml4);
        // Another *live* thread still in the old address space: only when
        // the caller skipped the de-thread (the suite does). The process's
        // timers stay where they are for it.
        let shared_live = old.is_some_and(|old| {
            tasks.iter().enumerate().any(|(other, task)| {
                other != me
                    && task
                        .as_ref()
                        .is_some_and(|task| task.pml4 == old && task.state != TaskState::Done)
            })
        });
        (old.filter(|old| *old != value), shared_live)
    };
    // Moved while this task still names the old table: once it lets go, the
    // reclamation of a finished sibling (on another CPU) may free the old
    // table and forget its signal and timer state at any moment.
    if let Some(old) = old {
        // A kill that reached the task while it loaded the new image must
        // still end it.
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
    let mut tasks = TASKS.lock();
    // Asked before the switch: the leader must share the thread's table.
    let leader = super::family::leader_of(&tasks, me);
    if let Some(task) = tasks[me].as_mut() {
        task.pml4 = value;
    }
    let carried = old
        .filter(|_| leader != me)
        .is_some_and(|old| carry_leader(&mut tasks, me, leader, old, value, shared_live));
    let orphaned = old.filter(|old| {
        !tasks
            .iter()
            .enumerate()
            .any(|(other, task)| other != me && task.as_ref().is_some_and(|task| task.pml4 == *old))
    });
    drop(tasks);
    // A kill that came after the carry above (a lookup: harmless when the old
    // state is already gone).
    if let Some(old) = old {
        signal::carry_kill(old, value);
        // The leader that came along is dead: what it held in the Messenger
        // fabric (channels, names, buffer handles, devices) goes now, not when
        // the new image ends, so its peers see it close. Its buffer mappings
        // stay recorded against the old table and go with it.
        if carried {
            crate::ipc::teardown_task(leader, old, true);
        }
        // Nothing runs in the old image any more: the buffer mappings made in
        // it (by this thread, or by a leader that moved with it) go now, not
        // when the old table is freed. Their handles stay open, and one that
        // this thread maps again from the new image gets a fresh address
        // instead of the old image's. Only the suite execs with a live
        // sibling left behind.
        if !shared_live {
            crate::ipc::shared::forget_table(old);
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

/// The leader of the exec'ing thread `me` follows it from `old` to `new`
/// when it is a zombie and nothing else lives in `old`: it holds the
/// process's pid, which the exec keeps (see [`set_pml4`]). Its old status is
/// not the process's (the de-thread killed it), so it reports the new
/// image's exit. A live leader (no de-thread) stays, and `me` becomes the
/// leader of a process of its own. Returns whether the leader moved.
fn carry_leader(
    tasks: &mut [Option<Task>; MAX_TASKS],
    me: usize,
    leader: usize,
    old: u64,
    new: u64,
    shared_live: bool,
) -> bool {
    let zombie = !shared_live
        && tasks[leader]
            .as_ref()
            .is_some_and(|task| task.state == TaskState::Done && task.pml4 == old);
    if zombie {
        if let Some(task) = tasks[leader].as_mut() {
            task.pml4 = new;
            task.exit_status = 0;
            task.linux.term_signal = 0;
        }
    } else if let Some(task) = tasks[me].as_mut() {
        task.linux.tgid = 0;
    }
    zombie
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
