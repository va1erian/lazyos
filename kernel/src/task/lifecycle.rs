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

/// Whether the current task has any children (the harness; `wait4` uses
/// [`has_child_filtered`]).
#[allow(dead_code)]
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
/// so it has no reaper. Returns the removed task and whether another task
/// still shares its address space (so the space must outlive this removal),
/// or `None` when nothing was removed.
///
/// The task is handed back rather than dropped here: dropping it closes its
/// descriptors, and the last reference to a pipe end wakes the peer's wait
/// queue, which takes the task table (queue-before-table order). Dropping it
/// under the caller's `TASKS` lock would spin on that lock forever with
/// interrupts off (issue #404). The kernel stack is a static array reused
/// with the slot, so it needs no freeing.
pub(super) fn take_finished(
    tasks: &mut [Option<Task>; MAX_TASKS],
    slot: usize,
) -> Option<(Task, bool)> {
    let reclaimable = tasks[slot]
        .as_ref()
        .is_some_and(|task| task.state == TaskState::Done && task.parent == 0);
    if !reclaimable {
        return None;
    }
    let dead = tasks[slot].take()?;
    runq::sync(tasks, slot);
    let pml4 = dead.pml4;
    let shared = tasks
        .iter()
        .enumerate()
        .any(|(other, task)| other != slot && task.as_ref().is_some_and(|task| task.pml4 == pml4));
    Some((dead, shared))
}

/// Flag `slot` for task-context reclamation if it holds a finished parentless
/// task. Called from the scheduler with the task table locked; the actual
/// freeing is deferred to [`reclaim_pending`] (see [`PENDING_RECLAIM`]).
pub(super) fn mark_finished(tasks: &[Option<Task>; MAX_TASKS], slot: usize) {
    let finished_parentless = tasks[slot]
        .as_ref()
        .is_some_and(|task| task.state == TaskState::Done && task.parent == 0);
    if finished_parentless {
        PENDING_RECLAIM.set(slot);
    }
}

/// Reclaim the finished parentless tasks the scheduler flagged: free their
/// slots, task-owned buffers and — when the removal leaves an address space
/// with no users — its pages, page tables and PML4 frame.
///
/// Must run with interrupts disabled in task context (a syscall entry or the
/// mux loop): dropping a dead task takes the heap lock, and unlike a preempted
/// task the current task holds none inside a critical section there.
///
/// Each slot is handled in its own critical section, and the dead task is
/// dropped only once the table is unlocked, for the reason [`take_finished`]
/// gives: its pipe ends may wake a peer, which re-takes the table (issue
/// #404). Interrupts are off and the CPU is single, so nothing changes the
/// table between two slots; a thread removed while a sibling is still
/// pending sees the space as shared, and the sibling's removal frees it.
pub fn reclaim_pending() {
    // A task the timer sweep ended has its descriptors closed here too.
    close_exited_fds();
    let pending = PENDING_RECLAIM.take();
    if pending.is_empty() {
        return;
    }
    let mut removed_any = false;
    for slot in pending.iter().filter(|slot| *slot != 0) {
        let Some((dead, shared)) = take_finished(&mut TASKS.lock(), slot) else {
            continue;
        };
        let pml4 = dead.pml4;
        // The table lock is released (the guard was a temporary): closing
        // the dead task's descriptors may now wake a peer safely.
        drop(dead);
        crate::process::forget_task_args(slot);
        // Release what the dead task still holds in the Messenger fabric
        // before its address space goes away (`ipc::teardown_task` explains
        // why the order matters).
        crate::ipc::teardown_task(slot, pml4, shared);
        if !shared {
            let pages = mem::user_table_frame_count(PhysAddr::new(pml4));
            let released = release_address_space(pml4);
            let stats = mem::frame_stats();
            serial_println!(
                "mem: reclaimed address space {pml4:#x}: {pages} pages, released {released} frames, {} free of {}",
                stats.free,
                stats.total
            );
        }
        removed_any = true;
    }
    if removed_any {
        // A `clone` sleeping on table pressure can return early. Queue before
        // task table order holds: no table lock is held here.
        wait::SLOT.notify_all();
    }
}

/// Close the descriptors of every finished task flagged in [`PENDING_CLOSE`],
/// as Linux does at exit: the last write end of a pipe gives its reader
/// end-of-file even while the writer waits, unreaped, as a zombie.
///
/// Must run in task context with the task table unlocked: dropping a pipe end
/// wakes its peer, which takes the table (queue-before-table order, issue
/// #404), and other descriptors take the heap lock. A slot that was reaped and
/// reused before this ran holds a live task and is skipped, so a new task's
/// descriptors are never closed.
///
/// Like `fd_close`, each closed descriptor leaves the epoll instances in the
/// table that registered it, unless a live task sharing the instance still
/// holds the same file under that number (see [`orphaned_interests`]).
/// Otherwise an epoll inherited across `fork` would keep the exited child's
/// pipe write end open and its reader would never see end-of-file.
pub fn close_exited_fds() {
    for slot in PENDING_CLOSE.take().iter() {
        let (fds, orphaned) = {
            let mut tasks = TASKS.lock();
            let fds = match tasks[slot].as_mut() {
                Some(task) if task.state == TaskState::Done => task.fds.take_all(),
                _ => continue,
            };
            let orphaned = orphaned_interests(&tasks, &fds);
            (fds, orphaned)
        };
        for (epoll, fd) in &orphaned {
            if let Some(entry) = fds.get(*fd) {
                Epoll::drop_fd_if(epoll, *fd, entry);
            }
        }
        drop(orphaned);
        drop(fds);
    }
}

/// The `(epoll, descriptor)` interests an exited task's table `dead` leaves
/// without an owner: for each epoll instance in `dead` and each descriptor
/// `dead` held, no live task holds both that instance and the same file at
/// that number.
fn orphaned_interests(
    tasks: &[Option<Task>; MAX_TASKS],
    dead: &FdTable,
) -> Vec<(Arc<Epoll>, usize)> {
    let mut orphaned = Vec::new();
    for (_, entry) in dead.iter() {
        let Fd::Epoll { epoll } = entry else {
            continue;
        };
        for (fd, held_dead) in dead.iter() {
            let still_owned = tasks
                .iter()
                .flatten()
                .filter(|task| task.state != TaskState::Done)
                .any(|task| {
                    task.fds.get(fd).is_some_and(|live| live.same_file(held_dead))
                        && task.fds.iter().any(
                            |(_, held)| matches!(held, Fd::Epoll { epoll: other } if Arc::ptr_eq(other, epoll)),
                        )
                });
            if !still_owned {
                orphaned.push((Arc::clone(epoll), fd));
            }
        }
    }
    orphaned
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
    reap_matching(|_, _| true).map(|(slot, status, _)| (slot, status))
}

/// Which children a Linux `wait4`/`waitid` accepts: one pid, any child, or the
/// members of one process group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildFilter {
    Any,
    Pid(usize),
    Group(usize),
}

impl ChildFilter {
    fn accepts(self, slot: usize, task: &Task) -> bool {
        match self {
            ChildFilter::Any => true,
            ChildFilter::Pid(pid) => slot == pid,
            ChildFilter::Group(pgid) => task.pgid == pgid,
        }
    }
}

/// Reap a finished child that `filter` accepts: `(pid, exit status, the
/// signal that ended it or 0)`.
pub fn reap_child_filtered(filter: ChildFilter) -> Option<(usize, u64, u8)> {
    reap_matching(|slot, task| filter.accepts(slot, task))
}

/// Whether the caller has any child (finished or not) that `filter` accepts:
/// `wait4` answers `ECHILD` otherwise.
pub fn has_child_filtered(filter: ChildFilter) -> bool {
    let me = current();
    let tasks = TASKS.lock();
    tasks.iter().enumerate().any(|(slot, task)| {
        task.as_ref()
            .is_some_and(|task| task.parent == me && slot != me && filter.accepts(slot, task))
    })
}

/// [`reap_child`] restricted to the child in `slot`: `None` while it still
/// runs (or is not a child of the caller). `execve` of a native program waits
/// on exactly the program it started, whatever else the caller has forked.
pub fn reap_child_slot(slot: usize) -> Option<u64> {
    reap_matching(|index, _| index == slot).map(|(_, status, _)| status)
}

/// Whether `slot` holds a not-yet-reaped child of the current task.
pub fn is_child(slot: usize) -> bool {
    let me = current();
    TASKS
        .lock()
        .get(slot)
        .and_then(|task| task.as_ref())
        .is_some_and(|task| task.parent == me && slot != me)
}

/// The reaping body: take the first finished child whose slot `wanted` accepts.
fn reap_matching(wanted: impl Fn(usize, &Task) -> bool) -> Option<(usize, u64, u8)> {
    let me = current();
    let (index, status, pml4, shared, dead) = {
        let mut tasks = TASKS.lock();
        let mut found = None;
        // Only finished tasks can be reaped: the run queues index them, in
        // ascending slot order like the full scan this replaced.
        for index in runq::done().iter().filter(|&index| index != KERNEL_TASK) {
            let finished = tasks[index]
                .as_ref()
                .map(|task| {
                    task.parent == me && task.state == TaskState::Done && wanted(index, task)
                })
                .unwrap_or(false);
            if finished {
                // INVARIANT: `finished` was just computed from this same
                // `tasks[index]` under `TASKS.lock()`, held continuously
                // since; on today's single-CPU scheduler nothing else can
                // clear the slot in between. Revisit this unwrap if/when SMP
                // (platform-plan.md S8) lets another core touch `TASKS`
                // concurrently with a lock that isn't held for the whole
                // read-then-use span.
                #[allow(clippy::unwrap_used)]
                let task = tasks[index].as_ref().unwrap();
                let status = task.exit_status;
                let pml4 = task.pml4;
                let shared = tasks.iter().enumerate().any(|(other, task)| {
                    other != index && task.as_ref().is_some_and(|task| task.pml4 == pml4)
                });
                let dead = tasks[index].take();
                runq::sync(&tasks, index);
                found = Some((index, status, pml4, shared, dead));
                break;
            }
        }
        found?
    };
    let signal = dead.as_ref().map_or(0, |task| task.linux.term_signal);
    // Dropping the dead task closes its descriptors, which may wake a peer
    // blocked on a pipe it held. Must happen with `TASKS` unlocked: pipe
    // release takes the wait-queue lock and then the task table.
    drop(dead);
    // Its argument blocks (syscall 9) die with it.
    crate::process::forget_task_args(index);
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
    Some((index, status, signal))
}
