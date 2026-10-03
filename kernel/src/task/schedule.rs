//! The scheduling decision behind both scheduler gates (`task::switch`):
//! bookkeeping, the signal sweep, task selection and the context switch.

use super::*;

/// Context switch: called from the scheduler ISRs with the interrupted `rsp`.
///
/// `tick` is 1 from the PIT gate and 0 from the voluntary-reschedule gate
/// ([`switch::yield_now`]). Only a real tick advances the clock, acknowledges
/// IRQ0 and charges CPU time (issue #338); deadline expiry and selection run
/// on both paths.
///
/// Returns the `rsp` to resume (the next task's saved context).
#[no_mangle]
pub extern "C" fn schedule(current_rsp: u64, tick: u32) -> u64 {
    let tick = tick != 0;
    if tick && crate::arch::timer::stale_tick() {
        // An APIC tick accepted before line 0 was masked: no tick happened
        // for the kernel, exactly as a masked 8259 line delivers nothing.
        return current_rsp;
    }
    if tick {
        // Periods lost to interrupts-off stretches are caught up here
        // (issue #344); a normal entry is exactly one.
        let periods = crate::arch::clock::periods_since_last();
        crate::arch::idt::TICKS.fetch_add(periods, Ordering::Relaxed);
        // SAFETY: `tick` is set only by `timer_isr`, i.e. we are in the tick
        // handler (PIT IRQ0 or the local APIC timer) and it is in service.
        unsafe { crate::arch::timer::end_of_tick() };
        // Decode i8042 bytes a long syscall collected (`input::ps2`) even if
        // the controller has no IRQ1 pending for them any more. Before the
        // task table lock: the keyboard path takes it.
        crate::input::ps2::service();
        crate::input::ps2::dispatch();
    }

    #[cfg(lazyos_tests)]
    harness::note_entry_flags();
    let cur = CURRENT.load(Ordering::Relaxed);
    if tick {
        diag::note_tick(cur, current_rsp);
        #[cfg(lazyos_tests)]
        harness::note_tick_locks();
    }
    let mut tasks = TASKS.lock();
    if let Some(task) = tasks[cur].as_mut() {
        task.rsp = current_rsp;
    }
    on_entry(&mut tasks, cur, tick);

    // Apply pending signals at the boundary back to user mode: a term/core
    // default marks the thread group Done, and a handler frame is written
    // into the saved interrupt frame of a task whose address space is the one
    // installed right now (the interrupted task's). Tasks in other address
    // spaces get their frame in `resume`, once their table is installed
    // (issue #375). This is what reaches native `int 0x80` programs, whose
    // syscall stub is outside the signal layer. Terminations are
    // post-processed once the table lock is dropped.
    // SAFETY: every `Task::rsp` is an interrupt frame saved by this ISR.
    let (sweep_finished, sweep_count) =
        unsafe { signal::sweep(&mut tasks, mem::kernel_table().as_u64()) };

    // Flag finished parentless tasks for reclamation: a thread or a
    // kernel-started program has no parent to `wait4` it, so its slot and
    // address space would otherwise leak (issue #133). The interrupted task is
    // left for the tick that switches away from it; `reclaim_pending` frees
    // the flagged slots from task context.
    for slot in 1..MAX_TASKS {
        if slot != cur {
            mark_finished(&tasks, slot);
        }
    }

    // Pick the highest class with a runnable task, then the fairest member
    // within it. A task that is blocked or done is never selected.
    // `select_next` falls back to `cur` when nothing is runnable at all;
    // resuming `cur` there just re-enters its wait loop instead of stalling
    // the CPU.
    let next = select_next(&mut tasks, cur);
    if next == cur {
        drop(tasks);
        signal::finish_sweep(&sweep_finished[..sweep_count]);
        return current_rsp;
    }

    // `cur` fully leaves the CPU on this tick: its finished slot can be
    // reclaimed too (from task context, on a later syscall or mux iteration).
    mark_finished(&tasks, cur);
    drop(tasks);
    signal::finish_sweep(&sweep_finished[..sweep_count]);
    // The live x87/SSE registers are `cur`'s user state (the kernel is
    // soft-float): park them before `install` loads the next task's.
    fpu::save(cur);
    resume(next, cur)
}

/// Install `next` and return the stack pointer to resume it with, delivering
/// the signals its own address space had to be installed for. If that
/// delivery ends the task (no frame can be built), pick again: a finished
/// task is never resumed into user mode. `cur` is the fallback when nothing
/// else is runnable, exactly as in `select_next`.
fn resume(mut next: usize, cur: usize) -> u64 {
    loop {
        let rsp = install(next);
        let Some(ended) = signal::deliver_on_resume(next) else {
            return rsp;
        };
        signal::finish_sweep(&[ended]);
        let mut tasks = TASKS.lock();
        next = select_next(&mut tasks, next);
        if !runnable(&tasks, next) {
            next = cur;
        }
    }
}

/// Make `slot` the current task: its address space, the ring-0 stack the next
/// user trap uses, and its thread pointer. Returns its saved `rsp`.
fn install(slot: usize) -> u64 {
    let (pml4, kstack_top, rsp, fs_base) = {
        let tasks = TASKS.lock();
        // INVARIANT: `slot` came from `select_next` (or is the interrupted
        // task), so it is occupied; `tasks` was locked continuously around
        // that selection on today's single-CPU scheduler. Revisit if SMP
        // (platform-plan.md S8) introduces a window where another core can
        // clear a slot without holding this same lock across the whole span.
        let task = tasks[slot].as_ref().unwrap();
        (task.pml4, task.kstack_top, task.rsp, task.fs_base)
    };
    CURRENT.store(slot, Ordering::Relaxed);
    // Switch address space and the ring0 stack used for the next user trap.
    mem::switch_to(PhysAddr::new(pml4));
    if kstack_top != 0 {
        gdt::set_kernel_stack(kstack_top);
        crate::arch::linux::set_kernel_stack(kstack_top);
    }
    // Restore this task's user thread pointer and floating-point registers.
    crate::arch::msr::write(crate::arch::msr::IA32_FS_BASE, fs_base);
    fpu::restore(slot);
    rsp
}

/// Per-entry bookkeeping shared by both scheduler gates (and the test
/// harness): charge a real tick to the task that consumed it, then time out
/// waiters whose deadline has passed.
///
/// Expiry runs here, on the scheduler's lock, so a timed-out task is runnable
/// before this entry's selection runs and the wait path needs no separate
/// timer callback. It also runs on voluntary entries: a deadline that passed
/// while the CPU was busy is honoured at the next scheduling decision.
pub(crate) fn on_entry(tasks: &mut [Option<Task>; MAX_TASKS], cur: usize, tick: bool) {
    if tick {
        charge_tick(tasks, cur);
    }
    let now = crate::arch::idt::TICKS.load(Ordering::Relaxed);
    expire_deadlines(tasks, now);
}

/// Book one timer period to whoever consumed it: the current task when it
/// was runnable, otherwise the idle counter.
///
/// There is no idle task. When nothing is runnable, `pick_next` resumes the
/// parked task so it can `hlt` in its wait loop, and the CPU sits there with
/// `CURRENT` pointing at a `Blocked` slot. Charging that slot would make a
/// sleeping task look busy and every monitor read 100 % load, so a tick that
/// lands on a blocked task is idle time (`IDLE_TICKS`) instead. Voluntary
/// entries consume no timer period and are not charged at all.
pub(crate) fn charge_tick(tasks: &mut [Option<Task>; MAX_TASKS], cur: usize) {
    match tasks[cur].as_mut() {
        Some(task) if !matches!(task.state, TaskState::Blocked { .. }) => {
            // Charge across ticks without a switch too, so `cpu_usage`
            // reports real per-task CPU time.
            task.cpu_ticks = task.cpu_ticks.saturating_add(1);
        }
        _ => {
            super::IDLE_TICKS.fetch_add(1, Ordering::Relaxed);
        }
    }
}
