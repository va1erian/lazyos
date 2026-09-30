//! Time versus scheduling (issue #338): only the PIT advances the clock.
//!
//! A park enters the scheduler through the voluntary gate
//! (`task::switch::yield_now`). It must not advance `TICKS`, send an EOI or
//! charge CPU time, but it must still expire deadlines and select a task.

use super::*;
use crate::task::{PriorityClass, TaskState, WaitKind, WakeReason};

/// Fresh table with the kernel task current and runnable: an earlier test
/// may have left it parked (`begin_call` parks its caller), and a real
/// yield from a parked task would hand the CPU to another slot.
fn fresh() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
}

/// Real voluntary entries run with interrupts disabled, so a pending PIT
/// interrupt cannot land in between and the tick counter is exact.
fn with_irqs_off<T>(body: impl FnOnce() -> T) -> T {
    x86_64::instructions::interrupts::without_interrupts(body)
}

/// Many real yields through the voluntary gate leave the tick counter and
/// the current task's CPU accounting untouched, and resume the caller.
pub fn yield_does_not_tick() -> Result<(), String> {
    fresh();
    const YIELDS: usize = 20_000;
    let (before, after, cpu_before, cpu_after) = with_irqs_off(|| {
        let before = task::ticks();
        let cpu_before = task::cpu_ticks(task::KERNEL_TASK);
        for _ in 0..YIELDS {
            task::switch::yield_now();
        }
        (
            before,
            task::ticks(),
            cpu_before,
            task::cpu_ticks(task::KERNEL_TASK),
        )
    });
    check!(
        after == before,
        "{YIELDS} voluntary entries moved TICKS {before} -> {after}"
    );
    check!(
        cpu_after == cpu_before,
        "{YIELDS} voluntary entries charged CPU ticks {cpu_before} -> {cpu_after}"
    );
    check!(
        task::current() == task::KERNEL_TASK,
        "the only runnable task was not resumed: current is {}",
        task::current()
    );
    // The harness twin of the two gates: only the tick path charges.
    task::harness::on_entry(false);
    check!(
        task::cpu_ticks(task::KERNEL_TASK) == cpu_after,
        "a voluntary entry charged a CPU tick"
    );
    task::harness::on_entry(true);
    check!(
        task::cpu_ticks(task::KERNEL_TASK) == cpu_after + 1,
        "a timer entry did not charge its CPU tick"
    );
    task::harness::reset();
    Ok(())
}

/// A tick that lands on a parked task is idle time, not CPU time. There is
/// no idle task: when nothing is runnable the parked task stays current
/// while the CPU halts, so charging it would make a quiet system read 100 %
/// busy. Soaks both directions (blocked, then runnable again) many times so
/// the counters are shown to move in lock-step with the state, never both.
pub fn tick_on_parked_task_is_idle() -> Result<(), String> {
    fresh();
    const TICKS: u64 = 10_000;
    let me = task::KERNEL_TASK;
    let (cpu_parked, idle_parked, cpu_running, idle_running, cpu_start, idle_start) =
        with_irqs_off(|| {
            let cpu_start = task::cpu_ticks(me);
            let idle_start = task::idle_ticks();
            task::set_blocked(true);
            for _ in 0..TICKS {
                task::harness::on_entry(true);
            }
            let parked = (task::cpu_ticks(me), task::idle_ticks());
            task::set_blocked(false);
            for _ in 0..TICKS {
                task::harness::on_entry(true);
            }
            let running = (task::cpu_ticks(me), task::idle_ticks());
            // Simulated ticks never advanced uptime: put the idle counter
            // back so it stays a share of `ticks()` for the sysinfo checks.
            task::IDLE_TICKS.store(idle_start, core::sync::atomic::Ordering::Relaxed);
            (
                parked.0, parked.1, running.0, running.1, cpu_start, idle_start,
            )
        });
    check!(
        cpu_parked == cpu_start,
        "{TICKS} ticks on a parked task charged it CPU time {cpu_start} -> {cpu_parked}"
    );
    check!(
        idle_parked == idle_start + TICKS,
        "{TICKS} ticks on a parked task moved IDLE_TICKS {idle_start} -> {idle_parked}"
    );
    check!(
        cpu_running == cpu_parked + TICKS,
        "{TICKS} ticks on a runnable task charged {} instead of {TICKS}",
        cpu_running - cpu_parked
    );
    check!(
        idle_running == idle_parked,
        "ticks on a runnable task moved IDLE_TICKS {idle_parked} -> {idle_running}"
    );
    task::harness::reset();
    Ok(())
}

/// Parks whose deadline has already passed return `TimedOut` through the
/// voluntary path alone: expiry runs on every scheduler entry, so the
/// waiter needs no PIT tick, and the parks still leave `TICKS` unchanged.
pub fn voluntary_park_expires_deadline() -> Result<(), String> {
    fresh();
    let me = task::current();
    let queue = task::wait::WaitQueue::new(WaitKind::Sleep);
    const PARKS: usize = 5_000;
    let (before, after, reasons) = with_irqs_off(|| {
        let before = task::ticks();
        let mut timed_out = 0usize;
        for _ in 0..PARKS {
            if queue.wait(me, Some(task::ticks())) == WakeReason::TimedOut {
                timed_out += 1;
            }
        }
        (before, task::ticks(), timed_out)
    });
    check!(
        reasons == PARKS,
        "{} of {PARKS} expired parks did not report TimedOut",
        PARKS - reasons
    );
    check!(
        after == before,
        "{PARKS} parks moved TICKS {before} -> {after}"
    );
    check!(queue.is_empty(), "a returned waiter stayed enqueued");
    check!(
        task::harness::state(me) == Some(TaskState::Runnable),
        "the caller is not runnable after its parks: {:?}",
        task::harness::state(me)
    );
    task::harness::reset();
    Ok(())
}

/// A voluntary entry by one task expires *another* task's passed deadline
/// (the deadline sweep is not tied to the PIT), while a lower-class waiter
/// that timed out does not steal the CPU from the yielding task.
pub fn voluntary_entry_sweeps_other_deadlines() -> Result<(), String> {
    fresh();
    let child = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    check!(
        task::set_priority(child, PriorityClass::Background),
        "set_priority failed"
    );
    let queue = task::wait::WaitQueue::new(WaitKind::Sleep);
    let now = task::ticks();
    queue.park(child, Some(now));
    let far = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    queue.park(far, Some(now + 1_000_000));
    with_irqs_off(task::switch::yield_now);
    check!(
        task::current() == task::KERNEL_TASK,
        "the interactive yielder lost the CPU to {}",
        task::current()
    );
    check!(
        task::harness::state(child) == Some(TaskState::Runnable)
            && task::harness::take_wake_reason(child) == Some(WakeReason::TimedOut),
        "a passed deadline was not expired by a voluntary entry: {:?}",
        task::harness::state(child)
    );
    check!(
        matches!(task::harness::state(far), Some(TaskState::Blocked { .. })),
        "a future deadline was expired early: {:?}",
        task::harness::state(far)
    );
    for slot in [child, far] {
        task::harness::finish(slot, 0);
    }
    while task::reap_child().is_some() {}
    task::harness::reset();
    Ok(())
}
