//! Task exit, reaping and address-space reclamation.

use super::*;

/// Mark the current task finished with an exit status and re-parent its
/// children to the kernel/init task (see [`process::finish`]).
pub fn finish_current(code: u64) {
    process::finish(current(), code);
}

/// Finish every task that shares the current address space: Linux's
/// `exit_group`, which ends the *thread group* rather than one thread. Returns
/// the `clear_child_tid` addresses of threads that had one so the caller can
/// zero them and futex-wake any joiner (the Linux shim does that part).
pub fn exit_thread_group(status: u64) -> Vec<u64> {
    let pml4 = TASKS.lock()[current()].as_ref().map(|task| task.pml4);
    let Some(pml4) = pml4 else {
        return Vec::new();
    };
    let (slots, tids) = {
        let tasks = TASKS.lock();
        let mut slots = Vec::new();
        let mut tids = Vec::new();
        for (slot, task) in tasks.iter().enumerate() {
            let Some(task) = task else {
                continue;
            };
            if slot == KERNEL_TASK || task.pml4 != pml4 || task.state == TaskState::Done {
                continue;
            }
            slots.push(slot);
            if task.clear_child_tid != 0 {
                tids.push(task.clear_child_tid);
            }
        }
        (slots, tids)
    };
    for slot in slots {
        process::finish(slot, status);
    }
    tids
}

/// The current task's process group id.
pub fn pgid() -> usize {
    process::pgid_of(current())
}

/// The current task's parent pid (0 = the kernel/init task).
pub fn ppid() -> usize {
    process::ppid_of(current())
}

/// Terminate every task in process group `pgid` (issue #59). Kept for the test
/// harness; per-signal termination goes through `task::signal`.
#[allow(dead_code)]
pub fn kill_group(pgid: usize) -> usize {
    process::kill_group(pgid)
}

/// Whether the current task has any children.
pub fn has_children() -> bool {
    let tasks = TASKS.lock();
    let me = current();
    tasks
        .iter()
        .flatten()
        .any(|task| task.parent == me && task.parent != 0)
}

/// Drop an address space's non-task state and release its user pages, page
/// tables and PML4 frame. The caller guarantees no live task references
/// `pml4`.
pub(super) fn release_address_space(pml4: u64) -> usize {
    // Give the per-uid user-memory charge this address space still holds back.
    crate::quota::forget_address_space(pml4);
    forget_bumps(pml4);
    signal::forget(pml4);
    mem::free_user_table(PhysAddr::new(pml4))
}

/// Remove `slot` if it holds a finished task that no `wait4` can ever collect:
/// a `clone(CLONE_VM)` thread or a kernel-started program has `parent == 0`,
/// so it has no reaper. Returns the removed task's address space and whether
/// another task still shares it (so the space must outlive this removal), or
/// `None` when nothing was removed.
pub(super) fn take_finished(
    tasks: &mut [Option<Task>; MAX_TASKS],
    slot: usize,
) -> Option<(u64, bool)> {
    let task = tasks[slot].as_ref()?;
    if task.state != TaskState::Done || task.parent != 0 {
        return None;
    }
    let pml4 = task.pml4;
    // Dropping the task frees its fds and output/input buffers; the kernel
    // stack is a static array reused with the slot, so it needs no freeing.
    tasks[slot] = None;
    let shared = tasks
        .iter()
        .enumerate()
        .any(|(other, task)| other != slot && task.as_ref().is_some_and(|task| task.pml4 == pml4));
    Some((pml4, shared))
}

/// Flag `slot` for task-context reclamation if it holds a finished parentless
/// task. Called from the scheduler with the task table locked; the actual
/// freeing is deferred to [`reclaim_pending`] (see [`PENDING_RECLAIM`]).
pub(super) fn mark_finished(tasks: &[Option<Task>; MAX_TASKS], slot: usize) {
    let finished_parentless = tasks[slot]
        .as_ref()
        .is_some_and(|task| task.state == TaskState::Done && task.parent == 0);
    if finished_parentless {
        PENDING_RECLAIM.fetch_or(1u64 << slot, Ordering::Relaxed);
    }
}

/// Reclaim the finished parentless tasks the scheduler flagged: free their
/// slots, task-owned buffers and — when the removal leaves an address space
/// with no users — its pages, page tables and PML4 frame.
///
/// Must run with interrupts disabled in task context (a syscall entry or the
/// mux loop): dropping a dead task takes the heap lock, and unlike a preempted
/// task the current task holds none inside a critical section there.
pub fn reclaim_pending() {
    let pending = PENDING_RECLAIM.swap(0, Ordering::Relaxed);
    if pending == 0 {
        return;
    }
    let mut tasks = TASKS.lock();
    let mut orphans = [0u64; MAX_TASKS];
    let mut orphan_count = 0;
    // (slot, its PML4, whether another task still shares that PML4).
    let mut removed = [(0usize, 0u64, false); MAX_TASKS];
    let mut removed_count = 0;
    for slot in 1..MAX_TASKS {
        if pending & (1u64 << slot) != 0 {
            if let Some((pml4, shared)) = take_finished(&mut tasks, slot) {
                removed[removed_count] = (slot, pml4, shared);
                removed_count += 1;
                if !shared {
                    orphans[orphan_count] = pml4;
                    orphan_count += 1;
                }
            }
        }
    }
    drop(tasks);
    // Release what the dead tasks still hold in the Messenger fabric before
    // their address spaces go away (`ipc::teardown_task` explains why the
    // order matters). The task table is unlocked: closing an endpoint wakes
    // waiters, which takes the wait-queue lock and then the table.
    for &(slot, pml4, shared) in &removed[..removed_count] {
        crate::ipc::teardown_task(slot, pml4, shared);
    }
    if removed_count > 0 {
        // A `clone` sleeping on table pressure can return early. Queue before
        // task table order holds: the lock above is already released.
        wait::SLOT.notify_all();
    }
    for &pml4 in &orphans[..orphan_count] {
        let pages = mem::user_table_frame_count(PhysAddr::new(pml4));
        let released = release_address_space(pml4);
        let stats = mem::frame_stats();
        serial_println!(
            "mem: reclaimed address space {pml4:#x}: {pages} pages, released {released} frames, {} free of {}",
            stats.free,
            stats.total
        );
    }
}

/// Take a finished child of the current task, freeing its slot and address
/// space. The address space is torn down only when the reaped child is its
/// last user: `clone(CLONE_VM)` threads share their creator's PML4 and would
/// otherwise be left with freed page tables.
///
/// The teardown runs after dropping the task lock: it is slow (and can lock
/// other state), while an interrupt here would otherwise self-deadlock on
/// `TASKS`.
pub fn reap_child() -> Option<(usize, u64)> {
    let me = current();
    let (index, status, pml4, shared, dead) = {
        let mut tasks = TASKS.lock();
        let mut found = None;
        for index in 1..MAX_TASKS {
            let finished = tasks[index]
                .as_ref()
                .map(|task| task.parent == me && task.state == TaskState::Done)
                .unwrap_or(false);
            if finished {
                // INVARIANT: `finished` was just computed from this same
                // `tasks[index]` under `TASKS.lock()`, held continuously
                // since; on today's single-CPU scheduler nothing else can
                // clear the slot in between. Revisit this unwrap if/when SMP
                // (platform-plan.md S8) lets another core touch `TASKS`
                // concurrently with a lock that isn't held for the whole
                // read-then-use span.
                let task = tasks[index].as_ref().unwrap();
                let status = task.exit_status;
                let pml4 = task.pml4;
                let shared = tasks.iter().enumerate().any(|(other, task)| {
                    other != index && task.as_ref().is_some_and(|task| task.pml4 == pml4)
                });
                let dead = tasks[index].take();
                found = Some((index, status, pml4, shared, dead));
                break;
            }
        }
        found?
    };
    // Dropping the dead task closes its descriptors, which may wake a peer
    // blocked on a pipe it held. Must happen with `TASKS` unlocked: pipe
    // release takes the wait-queue lock and then the task table.
    drop(dead);
    // The dead task's Messenger handles, buffers and mappings go before its
    // address space does.
    crate::ipc::teardown_task(index, pml4, shared);
    if !shared {
        let pages = mem::user_table_frame_count(PhysAddr::new(pml4));
        let released = release_address_space(pml4);
        let stats = mem::frame_stats();
        serial_println!(
            "mem: reaped task {index}: {pages} pages, released {released} frames, {} free of {}",
            stats.free,
            stats.total
        );
    }
    // A freed slot releases a `clone` sleeping on table pressure early.
    wait::SLOT.notify_all();
    Some((index, status))
}
