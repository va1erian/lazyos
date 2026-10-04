//! Reschedule on wake (docs/performance-plan.md, P1.1).
//!
//! A wake only marks a task `Runnable`; on its own it waited for the next
//! scheduler entry, which on an idle CPU is the next 100 Hz tick: the task
//! halted in its wait loop re-halts after the interrupt that woke someone
//! else. [`note_wake`] therefore raises a `need_resched` flag when the woken
//! task should run *now*:
//!
//! * the CPU is idle: the current task is blocked (halted in its wait loop)
//!   or done (halted in `exit`), so any runnable task beats it; or
//! * the woken task is in a strictly higher scheduling class than the
//!   running one, the same rule the scheduler applies at every entry.
//!
//! Inside one class nothing changes: the stride scheduler still shares the
//! CPU at ticks, so a stream of same-class wakes cannot turn into a stream of
//! context switches.
//!
//! [`preempt_point`] acts on the flag. It is called where the timer could
//! have switched tasks anyway and nothing is held: on the way out of every
//! device interrupt (`arch::idt`, `arch::irq_stubs`) and of every syscall
//! (the native gate and the Linux `syscall` stub). An interrupt that is taken
//! at all interrupted code running with interrupts on, which by the #382
//! rule holds no spin lock, so yielding there is exactly as safe as the
//! timer preempting it there; a syscall return holds nothing by
//! construction. The yield is the ordinary voluntary gate, so the task
//! resumes right after it (inside its interrupt handler or at its syscall
//! return) the next time it is picked.
//!
//! There is no dedicated idle task: the task the scheduler resumes when
//! nothing is runnable halts in its own wait loop, and the interrupt that
//! wakes anyone else now leaves that loop through [`preempt_point`]. An idle
//! task would add a slot, a kernel stack and special cases in accounting and
//! introspection for no latency gain on this single CPU.

use super::*;

/// A wake made a task more deserving of the CPU than the current one.
static NEED_RESCHED: AtomicBool = AtomicBool::new(false);

/// Record that `woken` became runnable while `cur` is on the CPU, raising the
/// flag if it should run before the next tick. Called with the task table
/// held (from `wake_task_with`).
pub(super) fn note_wake(tasks: &[Option<Task>; MAX_TASKS], woken: usize, cur: usize) {
    if woken == cur {
        // The current task woke itself (an interrupt stopped its halt): it
        // simply continues when the handler returns.
        return;
    }
    let Some(new) = tasks.get(woken).and_then(|task| task.as_ref()) else {
        return;
    };
    let outranks = match tasks.get(cur).and_then(|task| task.as_ref()) {
        Some(running) if running.state == TaskState::Runnable => {
            new.class.rank() > running.class.rank()
        }
        // Blocked in its wait loop, done in `exit`, or an empty slot: the CPU
        // is idle and any runnable task should have it.
        _ => true,
    };
    if outranks {
        NEED_RESCHED.store(true, Ordering::Relaxed);
    }
}

/// A selection is being made: whatever raised the flag is accounted for.
pub(super) fn clear() {
    NEED_RESCHED.store(false, Ordering::Relaxed);
}

/// Whether a wake asked for a reschedule that has not happened yet.
#[allow(dead_code)] // test hook
pub fn pending() -> bool {
    NEED_RESCHED.load(Ordering::Relaxed)
}

/// Give the CPU away if a wake asked for it.
///
/// Call only with interrupts off, holding no lock, at a point the timer
/// could have preempted (interrupt return, syscall return): see the module
/// docs.
pub fn preempt_point() {
    if NEED_RESCHED.load(Ordering::Relaxed) {
        switch::yield_now();
    }
}

/// Leave the CPU for good: the current task is finished (`Done`). Yields at
/// once, so the next task runs now instead of at the next tick (P1.5). When
/// nothing else is runnable the scheduler resumes this task, which halts
/// until an interrupt makes someone runnable; that interrupt's preemption
/// point (the CPU is idle) or the next tick switches away. Call with
/// interrupts off and no lock held (a syscall body).
pub fn exit_cpu() -> ! {
    loop {
        switch::yield_now();
        x86_64::instructions::interrupts::enable_and_hlt();
        x86_64::instructions::interrupts::disable();
    }
}

/// The Linux `syscall` stub's return hook, called after `linux_dispatch` with
/// the result saved on the task's kernel stack: deliver device interrupts
/// raised meanwhile (as the native gate does on entry), then let a task
/// either woke run.
#[no_mangle]
extern "C" fn linux_syscall_return() {
    crate::dev::intx::service();
    preempt_point();
}

// ---------------------------------------------------------------------------
// Where an interrupt landed (P1.2)
// ---------------------------------------------------------------------------

/// Per task: set while it is inside [`super::nap`] with interrupts on, from
/// just before the halt to just after interrupts are masked again. Per task,
/// not per CPU: a task switched away mid-nap (by the tick, or by an
/// interrupt's preemption point) resumes in the same window, and an
/// interrupt pending at that moment arrives the instant it is resumed with
/// interrupts on, before its `cli`. That window holds no lock either.
static NAPPING: [AtomicBool; MAX_TASKS] = [const { AtomicBool::new(false) }; MAX_TASKS];

/// [`super::nap`] is about to halt with interrupts on.
pub(super) fn nap_begin() {
    if let Some(flag) = NAPPING.get(super::current()) {
        flag.store(true, Ordering::Relaxed);
    }
}

/// [`super::nap`] masked interrupts again.
pub(super) fn nap_end() {
    if let Some(flag) = NAPPING.get(super::current()) {
        flag.store(false, Ordering::Relaxed);
    }
}

/// Whether the interrupt being handled stopped code that holds no kernel
/// lock: user code (`code_segment` is ring 3) or the current task inside
/// `nap`. Call with interrupts off, at the start of an interrupt handler or
/// a scheduler entry (before anything switches tasks).
///
/// This is the precondition for running the device bottom half
/// (`dev::intx::service`, which takes the claim, device-table and channel
/// locks and allocates) in interrupt context. Any other ring-0 code running
/// with interrupts on — the kernel task's loop above all — might hold one of
/// those locks, and the bottom half would then spin on it forever; such an
/// interrupt leaves the work to the next syscall, tick or mux pass.
pub fn interrupted_quiet_context(code_segment: u64) -> bool {
    let napping = NAPPING
        .get(super::current())
        .is_some_and(|flag| flag.load(Ordering::Relaxed));
    napping || crate::arch::fault::from_user(code_segment)
}
