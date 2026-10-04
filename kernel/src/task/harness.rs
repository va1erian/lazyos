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

/// Timer ticks that interrupted ring-0 code, and those of them that found a
/// lock held which non-preemptible code also takes (heap, console): each is
/// a preempted holder that a syscall could then spin on forever (issue #382).
static TICKS_IN_KERNEL: AtomicU64 = AtomicU64::new(0);
static PREEMPTED_HOLDERS: AtomicU64 = AtomicU64::new(0);

/// Called by the scheduler on every timer tick (test builds only).
pub fn note_tick_locks() {
    if !super::diag::last_tick_in_kernel() {
        return;
    }
    TICKS_IN_KERNEL.fetch_add(1, Ordering::Relaxed);
    if crate::mem::heap_locked() || crate::console::locked() {
        PREEMPTED_HOLDERS.fetch_add(1, Ordering::Relaxed);
    }
}

/// `(ticks_in_kernel, preempted_holders)` since the last call; resets both.
pub fn take_tick_lock_stats() -> (u64, u64) {
    (
        TICKS_IN_KERNEL.swap(0, Ordering::Relaxed),
        PREEMPTED_HOLDERS.swap(0, Ordering::Relaxed),
    )
}

/// Kernel entries (scheduler gates, the native syscall gate) whose Rust body
/// started with the direction flag set. Must stay 0: the entry stubs clear
/// DF before calling into Rust (issue #405), and the kernel's `memcpy`/
/// `memset` silently corrupt memory when they run with it set.
static DF_ENTRIES: AtomicU64 = AtomicU64::new(0);
/// Entries checked by [`note_entry_flags`].
static ENTRY_CHECKS: AtomicU64 = AtomicU64::new(0);

/// Called at the top of `schedule` and `syscall_dispatch` (test builds only).
pub fn note_entry_flags() {
    ENTRY_CHECKS.fetch_add(1, Ordering::Relaxed);
    if x86_64::registers::rflags::read().contains(x86_64::registers::rflags::RFlags::DIRECTION_FLAG)
    {
        DF_ENTRIES.fetch_add(1, Ordering::Relaxed);
    }
}

/// `(entries_checked, entries_with_df_set)` since the last call; resets both.
pub fn take_entry_flag_stats() -> (u64, u64) {
    (
        ENTRY_CHECKS.swap(0, Ordering::Relaxed),
        DF_ENTRIES.swap(0, Ordering::Relaxed),
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
        super::runq::sync_all(&tasks);
        removed
    };
    // Dropping removed tasks closes their pipe ends, which may notify a
    // wait queue; do it with `TASKS` unlocked (queue-before-table order).
    drop(removed);
    for slot in 1..super::MAX_TASKS {
        crate::process::forget_task_args(slot);
    }
}

/// Mark `index` finished, as if it had called `exit` (re-parenting its
/// children, like the real path).
pub fn finish(index: usize, code: u64) {
    super::process::finish(index, code);
}

/// Make `parent` the parent of `slot` (the child-exit doorbell tests give a
/// kernel thread children without a fork).
pub fn set_parent(slot: usize, parent: usize) {
    if let Some(task) = TASKS.lock()[slot].as_mut() {
        task.parent = parent;
    }
}

/// Point `current()` at `slot` without a context switch. The tests build
/// multi-level process trees with `spawn_fork`, which forks the current
/// task.
pub fn switch_current(slot: usize) {
    super::CURRENT.store(slot, core::sync::atomic::Ordering::Relaxed);
}

/// How many tasks hold the working-directory string of `slot` (its own
/// reference included); 0 while the task is still at the implicit root.
/// Lets a test prove that `fork` shares the string and that a task's exit or
/// its own `chdir` lets go of it.
pub fn cwd_holders(slot: usize) -> usize {
    TASKS.lock()[slot]
        .as_ref()
        .and_then(|task| task.cwd.as_ref())
        .map_or(0, alloc::sync::Arc::strong_count)
}

/// Give a test task the personality `spawn` would (`spawn_fork` makes Linux
/// tasks; the native syscall paths need native ones).
pub fn set_kind(index: usize, kind: super::Kind) {
    if let Some(task) = TASKS.lock()[index].as_mut() {
        task.kind = kind;
    }
}

/// The state of task `index`.
pub fn state(index: usize) -> Option<TaskState> {
    TASKS.lock()[index].as_ref().map(|task| task.state)
}

/// Force `index`'s state, without queueing it anywhere (the wake-rule tests
/// stage a blocked or done current task this way).
pub fn set_state(index: usize, state: TaskState) {
    let mut tasks = TASKS.lock();
    if let Some(task) = tasks[index].as_mut() {
        task.state = state;
        if let TaskState::Blocked { deadline, .. } = state {
            super::waiting::set_timer(index, deadline);
        }
    }
    super::runq::sync(&tasks, index);
}

/// Classify `fd` in another task's descriptor table, so a test can verify
/// `fork` inheritance without switching `current()`.
pub fn fd_kind_at(slot: usize, fd: usize) -> super::FdKind {
    let tasks = TASKS.lock();
    match tasks[slot].as_ref().and_then(|task| task.fds.get(fd)) {
        Some(entry) => match entry {
            super::Fd::Pty { .. } => super::FdKind::Pty,
            super::Fd::Closed => super::FdKind::Closed,
            super::Fd::Terminal => super::FdKind::Terminal,
            super::Fd::File { .. } => super::FdKind::File,
            super::Fd::Vfs { .. } => super::FdKind::Vfs,
            super::Fd::Pipe { .. } => super::FdKind::Pipe,
            super::Fd::Socket { .. } => super::FdKind::Socket,
            super::Fd::Event { .. } => super::FdKind::EventFd,
            super::Fd::Epoll { .. } => super::FdKind::Epoll,
            super::Fd::UnixListener { .. } => super::FdKind::Listener,
            super::Fd::Unbound { .. } => super::FdKind::Unbound,
            super::Fd::Inet { .. } => super::FdKind::Inet,
        },
        _ => super::FdKind::Closed,
    }
}

/// Slots in the current task's descriptor table (open or not): the bound a
/// test iterates to when it checks that every descriptor is closed.
pub fn fd_table_len() -> usize {
    TASKS.lock()[super::current()]
        .as_ref()
        .map_or(0, |task| task.fds.len())
}

/// Whether `fd` in another task's table has `FD_CLOEXEC`.
pub fn fd_cloexec_at(slot: usize, fd: usize) -> bool {
    let tasks = TASKS.lock();
    tasks[slot]
        .as_ref()
        .and_then(|task| task.fds.flags(fd))
        .is_some_and(|flags| flags & super::FD_CLOEXEC != 0)
}

/// The slot the scheduler would pick next, without switching to it or
/// advancing any pass (a pure query, so it is deterministic).
pub fn next_runnable() -> usize {
    let tasks = TASKS.lock();
    super::pick_next(&tasks, super::current())
}

/// Task `index`'s stride pass (virtual time).
pub fn pass(index: usize) -> Option<u64> {
    TASKS.lock()[index].as_ref().map(|task| task.pass)
}

/// Stage the current task as having used up its minimum slice (P6.2), so a
/// deserving same-class wake preempts it at once.
pub fn expire_slice() {
    super::preempt::set_selected(super::current(), 0);
}

/// Stage the current task as just selected: a deserving same-class wake is
/// deferred to the end of its minimum slice.
pub fn fresh_slice() {
    let end = crate::arch::clock::monotonic_ns() + super::preempt::MIN_SLICE_NS;
    super::preempt::set_selected(super::current(), end);
}

/// Whether a same-class preemption is deferred to the end of a slice.
pub fn deferred_pending() -> bool {
    super::preempt::deferred().is_some()
}

/// Set task `index`'s stride pass (the wake-rule tests stage who deserves
/// the CPU this way).
pub fn set_pass(index: usize, pass: u64) {
    if let Some(task) = TASKS.lock()[index].as_mut() {
        task.pass = pass;
    }
}

/// Compare the run queues with a full scan of the table; panics on the first
/// difference (`runq::verify`).
pub fn verify_runq() {
    super::runq::verify(&TASKS.lock());
}

/// The pick of the full-table scan the run queues replaced (P6.1): highest
/// class first, then the smallest pass, ties in round-robin order after the
/// current task. The suite checks `next_runnable` against it.
pub fn reference_pick() -> usize {
    let tasks = TASKS.lock();
    let cur = super::current();
    for class in PriorityClass::ALL.iter().rev() {
        let mut best: Option<(usize, u64)> = None;
        for step in 1..=super::MAX_TASKS {
            let slot = (cur + step) % super::MAX_TASKS;
            let Some(task) = tasks[slot].as_ref() else {
                continue;
            };
            if task.state != TaskState::Runnable || task.class != *class {
                continue;
            }
            if best.is_none_or(|(_, pass)| task.pass < pass) {
                best = Some((slot, task.pass));
            }
        }
        if let Some((slot, _)) = best {
            return slot;
        }
    }
    cur
}

/// Run one scheduling decision exactly as a timer tick would, without a
/// context switch: charge the current task a CPU tick, run the stride
/// selection, point `current()` at the winner, and return it. Tests use
/// this to simulate N ticks in kernel time (issue #58).
pub fn simulate_tick() -> usize {
    let mut tasks = TASKS.lock();
    let cur = super::current();
    super::schedule::charge_tick(&mut tasks, cur);
    // Same flagging the real tick does (issue #133): finished parentless
    // tasks are handed to `reclaim_pending`, the current one only when the
    // tick actually switches away from it.
    super::schedule::flag_finished(&tasks, cur);
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
    super::schedule::on_entry(&mut tasks, super::current(), tick);
}

/// The PML4 physical address of task `index`.
pub fn pml4(index: usize) -> Option<u64> {
    TASKS.lock()[index].as_ref().map(|task| task.pml4)
}

/// Run the deadline sweep with an explicit `now` in ticks, as the tick that
/// makes `TICKS` reach `now` would.
pub fn expire_deadlines(now: u64) {
    expire_deadlines_ns(super::ticks_to_ns(now));
}

/// Run the deadline sweep with an explicit `now` in monotonic nanoseconds.
pub fn expire_deadlines_ns(now: u64) {
    let mut tasks = TASKS.lock();
    super::expire_deadlines(&mut tasks, now);
}

/// Consume the recorded wake reason, as a wait loop does on resume.
pub fn take_wake_reason(index: usize) -> Option<WakeReason> {
    super::take_wake_reason(index)
}

/// Make `slot`'s saved frame look like a task preempted in user mode at
/// `rip` with stack pointer `user_rsp` (the frame's `CS` already says ring 3
/// for a spawned task). Returns whether the slot holds a task.
pub fn set_user_frame(slot: usize, rip: u64, user_rsp: u64) -> bool {
    let Some(frame) = frame_of(slot) else {
        return false;
    };
    // SAFETY: `frame` is the interrupt frame `spawn_fork` built on the slot's
    // own kernel stack; RIP and RSP sit at the timer-frame indices.
    unsafe {
        super::sys::put_frame_word(frame, super::signal::FRAME_RIP_INDEX, rip);
        super::sys::put_frame_word(frame, super::signal::FRAME_RIP_INDEX + 3, user_rsp);
    }
    true
}

/// Make `slot`'s saved frame say it was saved in ring 0, like the frame of a
/// task parked inside a syscall (the voluntary gate saves a kernel `CS`).
/// Returns the `CS` it replaced, for [`set_frame_cs`] to put back when the
/// test lets the task "return" to user mode.
pub fn set_kernel_frame(slot: usize) -> Option<u64> {
    use x86_64::instructions::segmentation::{Segment, CS};
    let frame = frame_of(slot)?;
    let kernel_cs = u64::from(CS::get_reg().0);
    let index = super::signal::FRAME_RIP_INDEX + 1;
    // SAFETY: `frame` is the interrupt frame the kernel built or saved on the
    // slot's own kernel stack; `CS` follows RIP in the timer-frame layout.
    unsafe {
        let old = super::sys::frame_word(frame, index);
        super::sys::put_frame_word(frame, index, kernel_cs);
        Some(old)
    }
}

/// Put `cs` back into `slot`'s saved frame (see [`set_kernel_frame`]).
pub fn set_frame_cs(slot: usize, cs: u64) -> bool {
    let Some(frame) = frame_of(slot) else {
        return false;
    };
    // SAFETY: as `set_kernel_frame`.
    unsafe { super::sys::put_frame_word(frame, super::signal::FRAME_RIP_INDEX + 1, cs) };
    true
}

/// The registers `slot` would resume with, read from its saved frame.
pub fn frame_regs(slot: usize) -> Option<super::signal::UserRegs> {
    let frame = frame_of(slot)?;
    // SAFETY: `frame` is an interrupt frame the kernel saved (see
    // `set_user_frame`), so it has the timer-frame layout.
    Some(unsafe { super::signal::regs_from_frame(frame, super::signal::FRAME_RIP_INDEX) })
}

/// The saved-frame pointer of `slot`.
fn frame_of(slot: usize) -> Option<u64> {
    TASKS.lock()[slot].as_ref().map(|task| task.rsp)
}

/// Run the scheduler's signal sweep once, exactly as a timer tick does with
/// the installed page table: default actions for every runnable task with a
/// user frame, handler frames for those in the installed address space, then
/// the post-lock side effects (`SIGCHLD`, `wait4` wakeups).
pub fn run_sweep() {
    let (finished, count) = {
        let mut tasks = TASKS.lock();
        // SAFETY: `tasks` is the live table and every `Task::rsp` in it is a
        // frame the kernel built (`spawn_*`) or saved (the scheduler ISRs).
        unsafe { super::signal::sweep(&mut tasks, crate::mem::kernel_table().as_u64()) }
    };
    super::signal::finish_sweep(&finished[..count]);
}

/// Deliver to `slot` as the scheduler does when it resumes it: with the
/// task's own table installed (restored afterwards). Returns whether the
/// delivery ended the task.
pub fn resume_delivery(slot: usize) -> bool {
    let Some(pml4) = pml4(slot) else {
        return false;
    };
    let previous = crate::mem::kernel_table();
    crate::mem::switch_to(x86_64::PhysAddr::new(pml4));
    let ended = super::signal::deliver_on_resume(slot);
    crate::mem::switch_to(previous);
    if let Some(ended) = ended {
        super::signal::finish_sweep(&[ended]);
    }
    ended.is_some()
}

/// What a task parked on a wait queue runs each time it wakes from a nap with
/// no wake reason yet. The suite has no second task to play a peer, so this is
/// how one acts *while* a blocking call waits: bytes that arrive later, or the
/// `Interrupted` wake a signal brings.
static NAP_HOOK: spin::Mutex<Option<fn()>> = spin::Mutex::new(None);

/// Install (or with `None` remove) the [`NAP_HOOK`].
pub fn set_nap_hook(hook: Option<fn()>) {
    *NAP_HOOK.lock() = hook;
}

/// Run the [`NAP_HOOK`], if any, with its lock released (the hook may wake
/// the waiter, which takes queue and task-table locks).
pub(super) fn run_nap_hook() {
    let hook = *NAP_HOOK.lock();
    if let Some(hook) = hook {
        hook();
    }
}

/// Wake `index` from its wait with `Interrupted`, as a deliverable signal
/// does (`signal::wake_blocked_threads` skips the kernel task, which the
/// suite runs as).
pub fn interrupt(index: usize) {
    super::wake_task_with(index, WakeReason::Interrupted);
}

/// Run `f` with the task table locked, as a syscall in the middle of task
/// bookkeeping would (the interrupt-window suite checks that no handler a
/// window admits ever needs it).
pub fn with_table_locked<R>(f: impl FnOnce() -> R) -> R {
    let _table = TASKS.lock();
    f()
}
