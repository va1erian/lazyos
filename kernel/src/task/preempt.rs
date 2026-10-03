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

/// The Linux `syscall` stub's return hook, called after `linux_dispatch` with
/// the result saved on the task's kernel stack.
#[no_mangle]
extern "C" fn linux_syscall_return() {
    preempt_point();
}
