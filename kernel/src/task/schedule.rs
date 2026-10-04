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
    let started = crate::perf::sched_enter();
    ENTRIES.fetch_add(1, Ordering::Relaxed);
    let rsp = decide(current_rsp, tick != 0);
    crate::perf::sched_exit(started);
    rsp
}

/// Scheduler entries since boot (ticks, parks and yields).
static ENTRIES: AtomicU64 = AtomicU64::new(0);
/// Entries that put a different task on the CPU.
static SWITCHES: AtomicU64 = AtomicU64::new(0);

/// Context switches since boot: scheduler entries that resumed a task other
/// than the one they interrupted (the idle-desktop rate, P6/P7).
#[allow(dead_code)] // read by the `PERF:ctxsw` report (LAZYOS_PERF=1)
pub fn context_switches() -> u64 {
    SWITCHES.load(Ordering::Relaxed)
}

/// Scheduler entries since boot, switching or not.
#[allow(dead_code)] // read by the `PERF:ctxsw` report (LAZYOS_PERF=1)
pub fn scheduler_entries() -> u64 {
    ENTRIES.load(Ordering::Relaxed)
}

/// The body of [`schedule`].
fn decide(current_rsp: u64, tick: bool) -> u64 {
    if tick && crate::arch::timer::stale_tick() {
        // An APIC tick accepted before line 0 was masked: no tick happened
        // for the kernel, exactly as a masked 8259 line delivers nothing.
        return current_rsp;
    }
    // SAFETY: `current_rsp` is the frame the gate just saved: 15 registers,
    // then RIP and CS.
    let code_segment = unsafe { sys::frame_word(current_rsp, 16) };
    let quiet = preempt::interrupted_quiet_context(code_segment);
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
        // Device interrupts raised while this tick's target held a lock wait
        // no longer than one tick that lands in user code or a nap (P1.2).
        if quiet {
            crate::dev::intx::service_in_interrupt();
        }
    }

    #[cfg(lazyos_tests)]
    harness::note_entry_flags();
    let preempted = preempt::take_preempting() && !tick;
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
    #[cfg(lazyos_tests)]
    runq::verify(&tasks);
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
    flag_finished(&tasks, cur);

    // Pick the highest class with a runnable task, then the fairest member
    // within it. A task that is blocked or done is never selected.
    // `select_next` falls back to `cur` when nothing is runnable at all;
    // resuming `cur` there just re-enters its wait loop instead of stalling
    // the CPU. A task parking right after a Messenger call or reply runs
    // its partner instead (P6.2, `preempt::hand_off`).
    let charged_at = preempt::open_charge(cur);
    let next = match preempt::take_handoff(&tasks, cur) {
        Some(partner) => {
            preempt::clear();
            charge(&mut tasks, partner);
            partner
        }
        None => select_next(&mut tasks, cur),
    };
    // `cur` leaves the CPU mid-quantum, preempted by a wake or parked: it
    // pays for what it used (P6.2, `preempt::refund`).
    let parked = tasks[cur]
        .as_ref()
        .is_some_and(|task| matches!(task.state, TaskState::Blocked { .. }));
    if let Some(at) = charged_at {
        if next != cur && (parked || (preempted && runnable(&tasks, cur))) {
            preempt::refund(&mut tasks, cur, at);
        }
    }
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
    SWITCHES.fetch_add(1, Ordering::Relaxed);
    // The live x87/SSE registers are `cur`'s user state (the kernel is
    // soft-float): park them before `install` loads the next task's.
    fpu::save(cur);
    resume(next, cur)
}

/// Flag every finished parentless task except `cur` for reclamation. Only
/// finished tasks are visited (the run queues' done mask, P6.1); zombies a
/// parent has yet to reap stay in it and are skipped by `mark_finished`.
pub(super) fn flag_finished(tasks: &[Option<Task>; MAX_TASKS], cur: usize) {
    for slot in runq::done().iter() {
        if slot != cur && slot != KERNEL_TASK {
            mark_finished(tasks, slot);
        }
    }
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
    crate::perf::on_run(slot);
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
    charge_window_ticks(tasks, cur);
    if tick {
        charge_tick(tasks, cur);
    }
    expire_deadlines(tasks, crate::arch::clock::monotonic_ns());
}

/// Expire what is due now: the deadline timer's interrupt
/// (`arch::event_timer`). Call with interrupts off and no lock held.
pub fn expire_due() {
    let mut tasks = TASKS.lock();
    expire_deadlines(&mut tasks, crate::arch::clock::monotonic_ns());
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

/// Charge the ticks taken inside interrupt windows (`arch::irq_window`),
/// which could not lock the table, to `cur`: windows open only in the running
/// task's syscall and never switch, so every one since the last scheduler
/// entry was `cur`'s, even if it has just blocked (a park is a voluntary
/// entry right after the syscall's work).
fn charge_window_ticks(tasks: &mut [Option<Task>; MAX_TASKS], cur: usize) {
    let ticks = crate::arch::irq_window::take_uncharged();
    if ticks == 0 {
        return;
    }
    match tasks[cur].as_mut() {
        Some(task) => task.cpu_ticks = task.cpu_ticks.saturating_add(ticks),
        None => {
            super::IDLE_TICKS.fetch_add(ticks, Ordering::Relaxed);
        }
    }
}
