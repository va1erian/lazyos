//! Test-harness hooks (issue #62), compiled only with `LAZYOS_TESTS=1`. They let
//! the in-kernel suite drive task bookkeeping without a running scheduler.

use super::{select_next, PriorityClass, TaskState, WakeReason, KERNEL_TASK, TASKS};
use core::sync::atomic::{AtomicU64, Ordering};

/// Critical sections (of locks an IRQ handler also takes) that were
/// entered with interrupts enabled. Must stay 0.
static IRQS_ON_IN_CRITICAL: AtomicU64 = AtomicU64::new(0);
/// Critical sections checked by [`note_critical_section`].
static CRITICAL_CHECKS: AtomicU64 = AtomicU64::new(0);

/// Record whether interrupts are enabled at the start of an IRQ-shared
/// critical section (`task::live`, `mouse::take_moved`, ...).
pub fn note_critical_section() {
    CRITICAL_CHECKS.fetch_add(1, Ordering::Relaxed);
    if x86_64::instructions::interrupts::are_enabled() {
        IRQS_ON_IN_CRITICAL.fetch_add(1, Ordering::Relaxed);
    }
}

/// `(checks, entered_with_irqs_on)` since the last call; resets both.
pub fn take_critical_stats() -> (u64, u64) {
    (
        CRITICAL_CHECKS.swap(0, Ordering::Relaxed),
        IRQS_ON_IN_CRITICAL.swap(0, Ordering::Relaxed),
    )
}

/// Free every slot except the kernel task's and zero its scheduler
/// accounting, so tests do not inherit virtual-time or CPU ticks from an
/// earlier test.
pub fn reset() {
    let removed: alloc::vec::Vec<super::Task> = {
        let mut tasks = TASKS.lock();
        let removed = tasks
            .iter_mut()
            .skip(1)
            .filter_map(|slot| slot.take())
            .collect();
        if let Some(task) = tasks[KERNEL_TASK].as_mut() {
            task.pass = 0;
            task.cpu_ticks = 0;
            task.class = PriorityClass::Interactive;
            task.weight = PriorityClass::Interactive.default_weight();
        }
        removed
    };
    // Dropping removed tasks closes their pipe ends, which may notify a
    // wait queue; do it with `TASKS` unlocked (queue-before-table order).
    drop(removed);
}

/// Mark `index` finished, as if it had called `exit` (re-parenting its
/// children, like the real path).
pub fn finish(index: usize, code: u64) {
    super::process::finish(index, code);
}

/// Point `current()` at `slot` without a context switch. The tests build
/// multi-level process trees with `spawn_fork`, which forks the current
/// task.
pub fn switch_current(slot: usize) {
    super::CURRENT.store(slot, core::sync::atomic::Ordering::Relaxed);
}

/// The state of task `index`.
pub fn state(index: usize) -> Option<TaskState> {
    TASKS.lock()[index].as_ref().map(|task| task.state)
}

/// Classify `fd` in another task's descriptor table, so a test can verify
/// `fork` inheritance without switching `current()`.
pub fn fd_kind_at(slot: usize, fd: usize) -> super::FdKind {
    let tasks = TASKS.lock();
    match tasks[slot].as_ref() {
        Some(task) if fd < super::FD_COUNT => match task.fds[fd] {
            super::Fd::Closed => super::FdKind::Closed,
            super::Fd::Terminal => super::FdKind::Terminal,
            super::Fd::File { .. } => super::FdKind::File,
            super::Fd::Pipe { .. } => super::FdKind::Pipe,
            super::Fd::Socket { .. } => super::FdKind::Socket,
            super::Fd::Event { .. } => super::FdKind::EventFd,
            super::Fd::Epoll { .. } => super::FdKind::Epoll,
            super::Fd::UnixListener { .. } => super::FdKind::Listener,
            super::Fd::Unbound { .. } => super::FdKind::Unbound,
        },
        _ => super::FdKind::Closed,
    }
}

/// Whether `fd` in another task's table has `FD_CLOEXEC`.
pub fn fd_cloexec_at(slot: usize, fd: usize) -> bool {
    let tasks = TASKS.lock();
    match tasks[slot].as_ref() {
        Some(task) if fd < super::FD_COUNT => {
            !matches!(task.fds[fd], super::Fd::Closed) && task.fd_flags[fd] & super::FD_CLOEXEC != 0
        }
        _ => false,
    }
}

/// The slot the scheduler would pick next, without switching to it or
/// advancing any pass (a pure query, so it is deterministic).
pub fn next_runnable() -> usize {
    let tasks = TASKS.lock();
    super::pick_next(&tasks, super::current())
}

/// Run one scheduling decision exactly as a timer tick would, without a
/// context switch: charge the current task a CPU tick, run the stride
/// selection, point `current()` at the winner, and return it. Tests use
/// this to simulate N ticks in kernel time (issue #58).
pub fn simulate_tick() -> usize {
    let mut tasks = TASKS.lock();
    let cur = super::current();
    if let Some(task) = tasks[cur].as_mut() {
        task.cpu_ticks = task.cpu_ticks.saturating_add(1);
    }
    // Same flagging the real tick does (issue #133): finished parentless
    // tasks are handed to `reclaim_pending`, the current one only when the
    // tick actually switches away from it.
    for slot in 1..super::MAX_TASKS {
        if slot != cur {
            super::mark_finished(&tasks, slot);
        }
    }
    let next = select_next(&mut tasks, cur);
    if next != cur {
        super::mark_finished(&tasks, cur);
    }
    super::CURRENT.store(next, core::sync::atomic::Ordering::Relaxed);
    next
}

/// Run the per-entry bookkeeping of a scheduler gate for the current task
/// (`tick` selects the PIT gate, otherwise the voluntary gate), without a
/// selection or a context switch (issue #338).
pub fn on_entry(tick: bool) {
    let mut tasks = TASKS.lock();
    super::on_entry(&mut tasks, super::current(), tick);
}

/// The PML4 physical address of task `index`.
pub fn pml4(index: usize) -> Option<u64> {
    TASKS.lock()[index].as_ref().map(|task| task.pml4)
}

/// Run the deadline sweep with an explicit `now`, as a timer tick would.
pub fn expire_deadlines(now: u64) {
    let mut tasks = TASKS.lock();
    super::expire_deadlines(&mut tasks, now);
}

/// Consume the recorded wake reason, as a wait loop does on resume.
pub fn take_wake_reason(index: usize) -> Option<WakeReason> {
    super::take_wake_reason(index)
}
