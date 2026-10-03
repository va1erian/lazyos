//! Reschedule on wake (docs/performance-plan.md, P1.1).
//!
//! `wake_task_with` raises `need_resched` when the woken task should run
//! before the next tick (the CPU is idle, or it is in a strictly higher
//! class), and interrupt and syscall returns act on it. The rules are checked
//! on the table alone; the switches are checked end to end with real kernel
//! threads (`task::kthread`), including a real interrupt (the COM1 UART's
//! transmit-empty line) waking a thread while the CPU is halted.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::*;
use crate::task::wait::WaitQueue;
use crate::task::{PriorityClass, TaskState, WaitKind};

mod irq_latency;

pub(super) use irq_latency::*;

/// Where the test threads park.
pub(super) static THREAD_QUEUE: WaitQueue = WaitQueue::new(WaitKind::Sleep);
/// Where the kernel task parks while a thread runs.
pub(super) static KERNEL_QUEUE: WaitQueue = WaitQueue::new(WaitKind::Sleep);
/// Passes the current test thread made through its loop.
pub(super) static RUNS: AtomicU64 = AtomicU64::new(0);
/// Slot of the current test thread.
pub(super) static THREAD: AtomicUsize = AtomicUsize::new(0);

/// Fresh table with the kernel task current, runnable and alone.
pub(super) fn fresh() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
    RUNS.store(0, Ordering::Relaxed);
}

/// The ping-pong thread: count a pass, then park until woken.
extern "C" fn counter() -> ! {
    let me = task::current();
    loop {
        RUNS.fetch_add(1, Ordering::Relaxed);
        THREAD_QUEUE.wait(me, None);
    }
}

/// Spawn `entry` in `class` and let it run to its first park.
pub(super) fn start_thread(entry: extern "C" fn() -> !, class: PriorityClass) -> Result<usize, String> {
    let slot = task::kthread::spawn_kernel_thread("perf-thread", entry, class)
        .map_err(|error| format!("spawn: {error}"))?;
    THREAD.store(slot, Ordering::Relaxed);
    // Let it run to its park, whatever its class: park the kernel task for
    // one yield so the scheduler must pick the thread.
    while !THREAD_QUEUE.contains(slot) {
        task::switch::yield_now();
    }
    Ok(slot)
}

/// End the thread, drain the queues, reap it and reset the table.
pub(super) fn stop_thread(slot: usize) -> Result<(), String> {
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::finish(slot, 0);
    THREAD_QUEUE.notify_all();
    KERNEL_QUEUE.notify_all();
    let reaped = task::reap_child();
    check!(reaped.is_some(), "the finished thread {slot} was not reapable");
    task::harness::reset();
    check!(
        !task::resched_pending(),
        "a reschedule request survived the teardown"
    );
    Ok(())
}

/// Park the current (kernel) task in `slot`'s place for a rule check, and
/// restore it after.
fn with_state<T>(slot: usize, state: TaskState, body: impl FnOnce() -> T) -> T {
    task::harness::set_state(slot, state);
    let out = body();
    task::harness::set_state(slot, TaskState::Runnable);
    out
}

fn blocked() -> TaskState {
    TaskState::Blocked {
        wait: WaitKind::Sleep,
        deadline: None,
    }
}

/// Clear the flag the way a scheduler entry does.
fn select() {
    let _ = task::harness::simulate_tick();
    task::harness::switch_current(task::KERNEL_TASK);
}

/// The rules: a higher class preempts, the same or a lower class waits for a
/// tick, an idle CPU takes anyone, a self-wake never asks, and a selection
/// clears the request.
pub fn wake_rules() -> Result<(), String> {
    fresh();
    let normal = task::spawn_fork().map_err(|e| format!("fork: {e}"))?;
    let realtime = task::spawn_fork().map_err(|e| format!("fork: {e}"))?;
    let peer = task::spawn_fork().map_err(|e| format!("fork: {e}"))?;
    task::set_priority(normal, PriorityClass::Normal);
    task::set_priority(realtime, PriorityClass::Realtime);
    task::set_priority(peer, PriorityClass::Interactive);
    let me = task::KERNEL_TASK;
    let wake = |slot: usize| {
        task::harness::set_state(slot, blocked());
        task::wake_task(slot)
    };
    select();

    check!(wake(normal), "the Normal task did not wake");
    check!(
        !task::resched_pending(),
        "a Normal wake asked to preempt the Interactive kernel task"
    );
    check!(wake(peer), "the peer did not wake");
    check!(
        !task::resched_pending(),
        "a same-class wake asked to preempt (it must wait for a tick)"
    );
    check!(wake(realtime), "the Realtime task did not wake");
    check!(
        task::resched_pending(),
        "a Realtime wake did not ask to preempt the Interactive kernel task"
    );
    select();
    check!(!task::resched_pending(), "a selection left the request set");

    // An idle CPU (the current task blocked or done) takes any class.
    let idle = with_state(me, blocked(), || wake(normal) && task::resched_pending());
    check!(idle, "a wake on an idle CPU did not ask to reschedule");
    select();
    let done = with_state(me, TaskState::Done, || wake(normal) && task::resched_pending());
    check!(done, "a wake while the current task is done did not ask");
    select();

    // The current task waking itself (its halt was interrupted) never asks.
    task::harness::switch_current(normal);
    let own = with_state(normal, blocked(), || {
        task::wake_task(normal) && !task::resched_pending()
    });
    task::harness::switch_current(me);
    check!(own, "a self-wake asked to reschedule");

    // A wake that finds nothing blocked changes nothing.
    check!(!task::wake_task(normal), "a runnable task woke again");
    check!(!task::resched_pending(), "a no-op wake asked to reschedule");
    for slot in [normal, realtime, peer] {
        task::harness::finish(slot, 0);
    }
    while task::reap_child().is_some() {}
    task::harness::reset();
    Ok(())
}

/// End to end, 100 000 times (200 000 context switches; a million rounds
/// takes about 400 s under TCG): the kernel task wakes a Realtime thread and
/// passes a preemption point; the thread must run at once (one pass per
/// wake, no lost and no extra wakeups) and park again, and nothing leaks
/// (queue entries, a stuck request). Then the same with a same-class thread,
/// which must *not* run until the kernel task gives the CPU up itself.
pub fn wake_yield_soak() -> Result<(), String> {
    const ROUNDS: u64 = 100_000;
    fresh();
    let slot = x86_64::instructions::interrupts::without_interrupts(|| {
        start_thread(counter, PriorityClass::Realtime)
    })?;
    let start_runs = RUNS.load(Ordering::Relaxed);
    let started = crate::perf::rdtsc();
    for round in 0..ROUNDS {
        check!(
            THREAD_QUEUE.notify_one() == 1,
            "round {round}: the thread was not parked"
        );
        check!(task::resched_pending(), "round {round}: no request raised");
        task::preempt_point();
        let runs = RUNS.load(Ordering::Relaxed) - start_runs;
        check!(
            runs == round + 1,
            "round {round}: the thread ran {runs} times, expected {}",
            round + 1
        );
        check!(
            !task::resched_pending() && THREAD_QUEUE.len() == 1,
            "round {round}: request {} queue {}",
            task::resched_pending(),
            THREAD_QUEUE.len()
        );
    }
    let cycles = crate::perf::rdtsc().wrapping_sub(started);
    serial_println!(
        "TEST:task_preempt_wake_yield_soak:INFO:rounds={ROUNDS} cycles_per_round={}",
        cycles / ROUNDS
    );
    check!(
        task::current() == task::KERNEL_TASK,
        "the soak ended on slot {}",
        task::current()
    );
    stop_thread(slot)?;

    // Same class: the wake must wait for the kernel task to give way.
    fresh();
    let slot = start_thread(counter, PriorityClass::Interactive)?;
    let base = RUNS.load(Ordering::Relaxed);
    for round in 0..1000u64 {
        THREAD_QUEUE.notify_one();
        task::preempt_point();
        check!(
            RUNS.load(Ordering::Relaxed) == base + round,
            "round {round}: a same-class wake preempted the kernel task"
        );
        // Give way the ordinary way. The stride scheduler may pick the
        // kernel task once more if its pass is behind; it cannot twice.
        for _ in 0..3 {
            task::switch::yield_now();
            if RUNS.load(Ordering::Relaxed) == base + round + 1 {
                break;
            }
        }
        check!(
            RUNS.load(Ordering::Relaxed) == base + round + 1,
            "round {round}: the woken peer did not run when the CPU was yielded"
        );
    }
    stop_thread(slot)
}

/// A thread that finishes at once, the way `exit` does.
extern "C" fn exiter() -> ! {
    RUNS.fetch_add(1, Ordering::Relaxed);
    task::finish_current(7);
    task::exit_cpu()
}

/// `exit` hands the CPU on at once (P1.5): 2000 Realtime threads each run,
/// finish and give the CPU back to the kernel task without waiting for a
/// timer tick, and each is reaped with its status. Before P1.5 an exiting
/// task halted until the next tick, so every round crossed one. The churn
/// also proves no slot leaks.
pub fn exit_hands_cpu_on() -> Result<(), String> {
    const ROUNDS: u64 = 2000;
    fresh();
    let free = task::free_slots();
    let mut crossed = 0u64;
    for round in 0..ROUNDS {
        let slot = task::kthread::spawn_kernel_thread("exiter", exiter, PriorityClass::Realtime)
            .map_err(|e| format!("round {round}: spawn: {e}"))?;
        let tick = task::ticks();
        // The Realtime thread outranks the kernel task: one yield runs it to
        // its exit, which must hand the CPU straight back.
        task::switch::yield_now();
        if task::ticks() != tick {
            crossed += 1;
        }
        check!(
            task::current() == task::KERNEL_TASK && RUNS.load(Ordering::Relaxed) == round + 1,
            "round {round}: current {} runs {}",
            task::current(),
            RUNS.load(Ordering::Relaxed)
        );
        check!(
            task::harness::state(slot) == Some(TaskState::Done),
            "round {round}: the thread is {:?}, not done",
            task::harness::state(slot)
        );
        let reaped = task::reap_child();
        check!(
            reaped.is_some_and(|(child, status)| child == slot && status == 7),
            "round {round}: reaped {reaped:?}"
        );
    }
    serial_println!("TEST:task_preempt_exit_hands_cpu_on:INFO:rounds={ROUNDS} crossed_tick={crossed}");
    check!(
        crossed <= ROUNDS / 10,
        "{crossed} of {ROUNDS} exits waited for a timer tick"
    );
    check!(
        task::free_slots() == free,
        "free slots {} -> {}",
        free,
        task::free_slots()
    );
    task::harness::reset();
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("task_preempt_wake_rules", wake_rules),
    ("task_preempt_wake_yield_soak", wake_yield_soak),
    ("task_preempt_irq_wake_idle_latency", irq_wake_idle_latency),
    ("task_preempt_exit_hands_cpu_on", exit_hands_cpu_on),
];
