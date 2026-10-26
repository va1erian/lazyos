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
///
/// The entry lock (`sync::entry`) is taken here on a tick from ring 3 and
/// inherited from ring 0; it is released iff the frame resumed is ring 3's,
/// so a switch between two kernel contexts carries it across.
#[no_mangle]
pub extern "C" fn schedule(current_rsp: u64, tick: u32) -> u64 {
    // SAFETY: both are frames the gate saved (or `decide` returns): 15
    // registers, then RIP and CS.
    let (rip, cs) = unsafe {
        (
            sys::frame_word(current_rsp, 15),
            sys::frame_word(current_rsp, 16),
        )
    };
    crate::sync::entry::enter(cs, rip);
    let started = crate::perf::sched_enter();
    ENTRIES.fetch_add(1, Ordering::Relaxed);
    let rsp = decide(current_rsp, tick != 0);
    crate::perf::sched_exit(started);
    // SAFETY: as above.
    crate::sync::entry::leave(unsafe { sys::frame_word(rsp, 16) });
    rsp
}

/// Scheduler entries since boot (ticks, parks and yields).
static ENTRIES: AtomicU64 = AtomicU64::new(0);
/// Installs that put a different task on a CPU than the last task it ran:
/// an idle stretch between two runs of the same task is not a switch, so
/// the count means what it meant before the idle context (S8.3b).
static SWITCHES: AtomicU64 = AtomicU64::new(0);
/// Per CPU: the last task (never an idle context) it installed.
static LAST_TASK: [AtomicU64; crate::arch::percpu::MAX_CPUS] =
    [const { AtomicU64::new(u64::MAX) }; crate::arch::percpu::MAX_CPUS];

/// Per slot: how many times the task left a CPU (`SYS:SLOW` tells a call
/// that let others run from one that monopolised its CPU by it).
static DEPARTURES: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];

/// How many times task `slot` has left a CPU since boot.
pub fn departures(slot: usize) -> u64 {
    DEPARTURES
        .get(slot)
        .map_or(0, |count| count.load(Ordering::Relaxed))
}

/// Count `slot`'s install in [`SWITCHES`] if it changes the task this CPU
/// runs (idle stretches aside).
fn note_switch(slot: usize) {
    if idle::is_idle(slot) {
        return;
    }
    let cpu = crate::arch::percpu::current().index() as usize;
    if LAST_TASK[cpu].swap(slot as u64, Ordering::Relaxed) != slot as u64 {
        SWITCHES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Context switches since boot: scheduler entries that left a CPU running a
/// different task than the last one it ran, idle stretches aside (the
/// idle-desktop rate, P6/P7).
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
    let boot_cpu = crate::arch::percpu::current().index() == 0;
    // Timer periods this entry stands for, for the interval timers' CPU
    // accounting.
    let tick_periods = if tick {
        tick_prologue(boot_cpu, quiet)
    } else {
        0
    };
    if !boot_cpu {
        idle::ap_tick_policy();
    }
    #[cfg(lazyos_tests)]
    harness::note_entry_flags();
    let preempted = preempt::take_preempting() && !tick;
    let cur = current();
    if tick && boot_cpu {
        diag::note_tick(cur, current_rsp);
        #[cfg(lazyos_tests)]
        harness::note_tick_locks();
    }
    if tick && diag::tick_finds_table_held(code_segment, current_rsp) {
        return current_rsp;
    }
    if tick {
        // Interval timers (setitimer/POSIX timers) fire before the table is
        // locked: queueing their signals takes it.
        itimer_tick(cur, tick_periods, code_segment);
    }
    itimer::fire_due_now();
    let mut tasks = TASKS.lock();
    if let Some(task) = tasks[cur].as_mut() {
        task.rsp = current_rsp;
        if tick && code_segment & 3 == 3 {
            task.guard.note_user_tick();
        }
    } else if idle::is_idle(cur) {
        idle::save_rsp(cur, current_rsp);
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
    if next != cur && runnable(&tasks, cur) {
        // Put back without a CPU: an idle one may take it now (S8.3c).
        kick::place(&tasks, cur);
    }
    if next == cur {
        drop(tasks);
        signal::finish_sweep(&sweep_finished[..sweep_count]);
        kick::flush_unlocked();
        return current_rsp;
    }

    // `cur` fully leaves the CPU on this tick: its finished slot can be
    // reclaimed too (from task context, on a later syscall or mux iteration).
    mark_finished(&tasks, cur);
    drop(tasks);
    signal::finish_sweep(&sweep_finished[..sweep_count]);
    // The live x87/SSE registers are `cur`'s user state (the kernel is
    // soft-float): park them before `install` loads the next task's. The
    // idle context has none (it never touches them, nor restores any).
    if !idle::is_idle(cur) {
        fpu::save(cur);
        segs::save(cur);
    }
    let rsp = resume(next, cur);
    // Once per entry: `resume` may install more than one pick.
    note_switch(current());
    // What this CPU woke or put back and did not take goes elsewhere now.
    kick::flush_unlocked();
    if oncpu::take_refused(cur) {
        // A reap of `cur` was refused while it ran here; it is current
        // nowhere now, so its parent may look again (S8.3c).
        tell_parent_again(cur);
    }
    rsp
}

/// `slot` finished while current on this CPU and a reap was refused: the
/// `wait4` sleepers and the parent's child bell hear of it once more (the
/// `SIGCHLD` was posted at the death and is not repeated).
fn tell_parent_again(slot: usize) {
    let parent = TASKS.lock()[slot].as_ref().map(|task| task.parent);
    if let Some(parent) = parent {
        super::childbell::ring(parent);
    }
    wait::CHILD_EXIT.notify_all();
}

/// A tick's machine-wide work, before the task table is locked; returns the
/// periods it stands for. Only the boot CPU's tick (the PIT or its APIC
/// timer) is the clock: it catches up lost periods (issue #344), advances
/// `TICKS`, drains the i8042 and runs the device bottom half (devices
/// interrupt the boot CPU). An AP's tick (its own APIC timer, S8.3c) is
/// one period of scheduling and accounting for that CPU, nothing more.
fn tick_prologue(boot_cpu: bool, quiet: bool) -> u64 {
    let periods = if boot_cpu {
        crate::arch::clock::periods_since_last()
    } else {
        1
    };
    if boot_cpu {
        crate::arch::idt::TICKS.fetch_add(periods, Ordering::Relaxed);
    }
    // SAFETY: a tick entry comes only from `timer_isr`, i.e. we are in the
    // tick handler (PIT IRQ0 or a local APIC timer) and it is in service.
    unsafe { crate::arch::timer::end_of_tick() };
    if boot_cpu {
        // Decode i8042 bytes a long syscall collected (`input::ps2`) even
        // if the controller has no IRQ1 pending for them any more. Before
        // the task table lock: the keyboard path takes it.
        crate::input::ps2::service();
        crate::input::ps2::dispatch();
        // Device interrupts raised while this tick's target held a lock
        // wait no longer than one tick that lands in user code or a nap
        // (P1.2).
        if quiet {
            crate::dev::intx::service_in_interrupt();
        }
    }
    periods
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
/// task is never resumed into user mode. With nothing else runnable the
/// fallback is this CPU's idle context ([`sched::fallback`]), exactly as in
/// `select_next`.
fn resume(mut next: usize, cur: usize) -> u64 {
    loop {
        // The sweep's group exit (`finish_sweep`) runs after `next` was
        // chosen and may have ended it: pick again rather than resume a
        // finished task into user mode.
        {
            let mut tasks = TASKS.lock();
            if next != cur && !idle::is_idle(next) && !runnable(&tasks, next) {
                next = select_next(&mut tasks, cur);
                if !runnable(&tasks, next) {
                    next = sched::fallback(cur);
                }
            }
        }
        let rsp = install(next);
        if idle::is_idle(next) {
            // No signals, no address space of its own.
            return rsp;
        }
        let Some(ended) = signal::deliver_on_resume(next) else {
            // The delivery may have stopped the task (a default stop parks
            // it): resuming it would run a task the table says is blocked.
            let mut tasks = TASKS.lock();
            if runnable(&tasks, next) {
                return rsp;
            }
            let other = select_next(&mut tasks, next);
            next = if other != next && runnable(&tasks, other) {
                other
            } else {
                sched::fallback(next)
            };
            if next == current() {
                // No idle context on this CPU yet: the stopped task naps in
                // its wait loop, as before S8.3b.
                return rsp;
            }
            continue;
        };
        signal::finish_sweep(&[ended]);
        let mut tasks = TASKS.lock();
        next = select_next(&mut tasks, next);
        if !runnable(&tasks, next) {
            next = sched::fallback(cur);
        }
    }
}

/// Make `slot` the current task: its address space, the ring-0 stack the next
/// user trap uses, and its thread pointer. Returns its saved `rsp`. An idle
/// context (`task::idle`) runs on the boot table and never enters ring 3.
fn install(slot: usize) -> u64 {
    let prev = current();
    let (pml4, kstack_top, rsp, fs_base, prev_runnable) = {
        let tasks = TASKS.lock();
        let prev_runnable = runnable(&tasks, prev);
        if idle::is_idle(slot) {
            (
                mem::boot_table().as_u64(),
                0,
                idle::rsp(slot),
                0,
                prev_runnable,
            )
        } else {
            // INVARIANT: `slot` came from `select_next` (or is the
            // interrupted task), so it is occupied; `tasks` was locked
            // continuously around that selection, and every CPU's scheduler
            // runs under the entry lock (`sync::entry`), so no other CPU can
            // clear the slot in between.
            #[allow(clippy::unwrap_used)]
            let task = tasks[slot].as_ref().unwrap();
            (
                task.pml4,
                task.kstack_top,
                task.rsp,
                task.fs_base,
                prev_runnable,
            )
        }
    };
    set_current(slot);
    if prev != slot {
        if let Some(count) = DEPARTURES.get(prev) {
            count.fetch_add(1, Ordering::Relaxed);
        }
    }
    if !idle::is_idle(slot) {
        crate::perf::on_run(slot, prev, prev_runnable);
    }
    // Switch address space and the ring0 stack used for the next user trap.
    mem::switch_to(PhysAddr::new(pml4));
    if kstack_top != 0 {
        gdt::set_kernel_stack(kstack_top);
        crate::arch::linux::set_kernel_stack(kstack_top);
    }
    // Its data segment selectors (before the FS base: loading FS resets it),
    // then its user thread pointer and floating-point registers.
    segs::restore(slot);
    crate::arch::msr::write(crate::arch::msr::IA32_FS_BASE, fs_base);
    if !idle::is_idle(slot) {
        fpu::restore(slot);
    }
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
    itimer::fire_due_now();
    let mut tasks = TASKS.lock();
    expire_deadlines(&mut tasks, crate::arch::clock::monotonic_ns());
}

/// Book one timer period to whoever consumed it: the current task when it
/// was runnable, otherwise the idle counter.
///
/// When nothing is runnable the CPU runs its idle context (`task::idle`),
/// whose slot the table leaves empty: a tick that lands there is idle time
/// (`note_idle_ticks`). So is a tick on a blocked or finished slot, which only
/// the suite's harness makes current now. Voluntary entries consume no
/// timer period and are not charged at all.
pub(crate) fn charge_tick(tasks: &mut [Option<Task>; MAX_TASKS], cur: usize) {
    match tasks[cur].as_mut() {
        // A `Done` task stays `cur` as the idle fallback (`pick_next`, the
        // `hlt` loop of `exit_cpu`) until something else is runnable: it is
        // not running, so it is charged nothing (issue #860).
        Some(task) if !matches!(task.state, TaskState::Blocked { .. } | TaskState::Done) => {
            // Charge across ticks without a switch too, so `cpu_usage`
            // reports real per-task CPU time.
            task.cpu_ticks = task.cpu_ticks.saturating_add(1);
            // ...and to its uid's CPU quota (issue #483).
            crate::quota::cpu::charge_ticks(cur, 1);
            super::guard::on_tick(tasks, cur, true);
        }
        _ => {
            super::note_idle_ticks(1);
            super::guard::on_tick(tasks, cur, false);
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
        Some(task) => {
            task.cpu_ticks = task.cpu_ticks.saturating_add(ticks);
            crate::quota::cpu::charge_ticks(cur, ticks);
        }
        None => {
            super::note_idle_ticks(ticks);
            return;
        }
    }
    // The guard counts them too: a long syscall that runs with interrupts
    // off was invisible to it. Bounded to one window boundary: a call that
    // took a second is a window of budget, not a hundred (`on_window_ticks`).
    super::guard::on_window_ticks(tasks, cur, ticks);
}

/// A tick landed in the code segment `code_segment` while `cur` was the
/// current task: book the period to its process's CPU-time interval timers
/// (`ITIMER_VIRTUAL` counts ring 3 only; `ITIMER_PROF` both), exactly the
/// way [`charge_tick`] books the task's own CPU time: a task that is
/// blocked (the CPU idling in its wait loop) is charged nothing.
fn itimer_tick(cur: usize, periods: u64, code_segment: u64) {
    if itimer::cpu_timers_armed() == 0 {
        return;
    }
    let pml4 = {
        let tasks = TASKS.lock();
        match tasks[cur].as_ref() {
            Some(task) if !matches!(task.state, TaskState::Blocked { .. } | TaskState::Done) => {
                Some(task.pml4)
            }
            _ => None,
        }
    };
    if let Some(pml4) = pml4 {
        itimer::charge_cpu(pml4, ticks_to_ns(periods.max(1)), code_segment & 3 == 3);
    }
}

/// An interval or POSIX timer was armed or removed: re-evaluate the one-shot
/// deadline timer so a deadline inside this tick period gets its interrupt.
pub fn itimer_changed() {
    let _tasks = TASKS.lock();
    sched::rearm_event_timer();
}

/// Test hook: the tick path's CPU-timer accounting for the current task as
/// if `periods` ticks landed in ring 3 (`user`) or ring 0.
#[cfg(lazyos_tests)]
pub fn itimer_tick_for_test(periods: u64, user: bool) {
    itimer_tick(current(), periods, if user { 3 } else { 0 });
}
