//! Idle contexts, on-CPU marks and halting without the entry lock
//! (docs/smp-plan.md S8.3b; `task::idle`, `task::oncpu`,
//! `sync::entry::halt`).
//!
//! Correctness: the idle slots are reserved and empty in the table; with
//! every task asleep the CPU runs its idle context on the boot table and no
//! task is current; a task marked current on another CPU is never selected,
//! handed the CPU or reaped. With `LAZYOS_SMP=1`: a halt drops the hold (an
//! AP waiting for the lock gets it inside the boot CPU's halt), and a ring-0
//! entry into a marked halt takes the lock. Stress: thousands of switches
//! into and out of the idle context (a sleeper and a ping-pong thread), and
//! the boot CPU halting over and over while the APs hammer the lock.

use core::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::task::wait::WaitQueue;
use crate::task::{idle, oncpu, PriorityClass, WaitKind, WakeReason};

const COMMON: &[(&str, Test)] = &[
    ("smp_idle_slots_are_reserved", slots_are_reserved),
    ("smp_idle_runs_while_everything_sleeps", runs_while_asleep),
    ("smp_idle_oncpu_mark_guards_a_task", oncpu_mark_guards),
    ("smp_idle_switch_soak", switch_soak),
    ("smp_idle_takes_over_from_an_exit", takes_over_from_an_exit),
];

#[cfg(not(lazyos_smp))]
pub(super) const CASES: &[(&str, Test)] = COMMON;
#[cfg(lazyos_smp)]
pub(super) const CASES: &[(&str, Test)] = &[
    COMMON[0],
    COMMON[1],
    COMMON[2],
    COMMON[3],
    COMMON[4],
    ("smp_idle_halt_drops_the_hold", locked::halt_drops_the_hold),
    ("smp_idle_fault_in_a_halt_takes_it", locked::fault_in_halt),
    (
        "smp_idle_entry_into_a_halt_takes_it",
        locked::entry_into_halt,
    ),
    (
        "smp_idle_halts_under_contention",
        locked::halts_under_contention,
    ),
];

/// Ping-pong rounds of the switch soak (`--soak`: the long one).
const SOAK_ROUNDS: u64 = if option_env!("LAZYOS_SMP_SOAK").is_some() {
    20_000
} else {
    500
};

/// The idle loop's pass count when the exiting thread finished.
static EXITED_AT: AtomicU64 = AtomicU64::new(u64::MAX);

/// A thread that ends at once: finished, then off the CPU for good.
extern "C" fn quitter() -> ! {
    let me = task::current();
    EXITED_AT.store(idle::probe::loops(), Ordering::Relaxed);
    task::harness::finish(me, 0);
    task::exit_cpu()
}

/// A thread exits while the kernel task sleeps: nothing is runnable, so the
/// idle context takes the CPU (not the finished thread, which the old
/// fallback halted in), the thread is current nowhere, and it is reaped.
fn takes_over_from_an_exit() -> Result<(), String> {
    fresh();
    EXITED_AT.store(u64::MAX, Ordering::Relaxed);
    let thread = task::kthread::spawn_kernel_thread("idle-quit", quitter, PriorityClass::Normal)
        .map_err(|error| format!("spawn: {error}"))?;
    nap_ns(30_000_000);
    let exited = EXITED_AT.load(Ordering::Relaxed);
    let state = task::harness::state(thread);
    let mark = oncpu::cpu_of(thread);
    let reaped = task::reap_child().map(|(slot, _)| slot);
    task::harness::reset();
    check!(exited != u64::MAX, "the thread never ran");
    check!(
        idle::probe::loops() > exited,
        "the idle loop never ran after the exit"
    );
    check!(
        state == Some(crate::task::TaskState::Done),
        "the thread is {state:?}"
    );
    check!(
        mark.is_none(),
        "the finished thread is current on CPU{mark:?}"
    );
    check!(reaped == Some(thread), "reaped {reaped:?}, not {thread}");
    Ok(())
}

/// Where the soak's thread parks, and its passes.
static THREAD_QUEUE: WaitQueue = WaitQueue::new(WaitKind::Sleep);
static THREAD_RUNS: AtomicU64 = AtomicU64::new(0);

/// A fresh table with the kernel task current and runnable.
fn fresh() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
}

/// The idle slots: one per CPU at the top of the table, never claimable,
/// empty in the table, and this CPU's exists once a kernel task registered.
fn slots_are_reserved() -> Result<(), String> {
    fresh();
    let slot = idle::here().ok_or("no idle context after register_kernel")?;
    let cpu = crate::arch::percpu::current().index() as usize;
    check!(
        slot == idle::FIRST_IDLE_SLOT + cpu,
        "CPU{cpu}'s idle slot is {slot}"
    );
    check!(idle::is_idle(slot), "{slot} is not an idle slot");
    check!(
        !idle::is_idle(idle::TASK_SLOTS - 1),
        "the last task slot counts as idle"
    );
    check!(
        task::harness::state(slot).is_none(),
        "the table holds a task in idle slot {slot}"
    );
    check!(
        crate::limits::task_max() <= idle::TASK_SLOTS,
        "task_max {} reaches into the idle slots",
        crate::limits::task_max()
    );
    Ok(())
}

/// The kernel task sleeps with nothing else runnable: the CPU runs its
/// idle context, on the boot table, with the kernel task current nowhere;
/// the sleep then ends on time and the kernel task is current again.
fn runs_while_asleep() -> Result<(), String> {
    fresh();
    let before = idle::probe::loops();
    let deadline = crate::arch::clock::monotonic_ns() + 30_000_000;
    let reason = task::wait_sleep_ns(deadline);
    check!(
        reason == WakeReason::TimedOut,
        "the sleep ended with {reason:?}"
    );
    check!(
        idle::probe::loops() > before,
        "the idle loop never ran while the only task slept"
    );
    let boot = crate::mem::boot_table().as_u64();
    check!(
        idle::probe::cr3() == boot,
        "the idle loop ran on {:#x}, not the boot table {boot:#x}",
        idle::probe::cr3()
    );
    check!(
        idle::probe::kernel_mark().is_none(),
        "the sleeping kernel task was still current on CPU{:?}",
        idle::probe::kernel_mark()
    );
    check!(
        task::current() == task::KERNEL_TASK,
        "woke as slot {}",
        task::current()
    );
    let cpu = crate::arch::percpu::current().index() as usize;
    check!(
        oncpu::cpu_of(task::KERNEL_TASK) == Some(cpu),
        "the running kernel task is marked {:?}",
        oncpu::cpu_of(task::KERNEL_TASK)
    );
    Ok(())
}

/// A task marked current on another CPU: the mark follows a switch here,
/// and while it names another CPU the task is never selected, never handed
/// the CPU, and not reaped once finished.
fn oncpu_mark_guards() -> Result<(), String> {
    fresh();
    let a = task::spawn_fork().map_err(|e| format!("fork: {e}"))?;
    let result = oncpu_mark_steps(a);
    // Whatever failed: a mark left naming another CPU would keep the slot
    // from ever being selected or reaped again.
    oncpu::set_for_test(a, None);
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
    task::harness::reset();
    result
}

fn oncpu_mark_steps(a: usize) -> Result<(), String> {
    let cpu = crate::arch::percpu::current().index() as usize;
    let other = cpu + 1;
    task::harness::switch_current(a);
    check!(
        oncpu::cpu_of(a) == Some(cpu) && oncpu::cpu_of(task::KERNEL_TASK).is_none(),
        "a switch left marks {:?} / {:?}",
        oncpu::cpu_of(a),
        oncpu::cpu_of(task::KERNEL_TASK)
    );
    task::harness::switch_current(task::KERNEL_TASK);
    // `a` is the only runnable task but the kernel task, which steps aside.
    oncpu::set_for_test(a, Some(other));
    task::set_blocked(true);
    let picked = task::harness::next_runnable();
    check!(picked != a, "selected {a}, current on CPU{other}");
    check!(
        Some(picked) == idle::here(),
        "picked {picked}, not the idle context"
    );
    task::hand_off(a);
    check!(
        task::harness::take_handoff().is_none(),
        "handed the CPU to {a}, current on CPU{other}"
    );
    oncpu::set_for_test(a, None);
    check!(
        task::harness::next_runnable() == a,
        "{a} not selected once its mark was cleared"
    );
    task::hand_off(a);
    check!(
        task::harness::take_handoff() == Some(a),
        "no handoff to {a} once its mark was cleared"
    );
    task::set_blocked(false);
    // Finished but current elsewhere: its stack is live there.
    oncpu::set_for_test(a, Some(other));
    task::harness::finish(a, 0);
    check!(
        task::reap_child().is_none(),
        "reaped {a} while current on CPU{other}"
    );
    oncpu::set_for_test(a, None);
    check!(
        task::reap_child().map(|(slot, _)| slot) == Some(a),
        "{a} not reaped once its mark was cleared"
    );
    Ok(())
}

/// The soak's thread: count a pass, then park until woken.
extern "C" fn pinger() -> ! {
    let me = task::current();
    loop {
        THREAD_RUNS.fetch_add(1, Ordering::Relaxed);
        THREAD_QUEUE.wait(me, None);
    }
}

/// [`SOAK_ROUNDS`] rounds of: wake the thread, sleep 200 µs. The thread
/// runs and parks, then nothing is runnable until the deadline, so every
/// round switches into the idle context and out again.
fn switch_soak() -> Result<(), String> {
    fresh();
    THREAD_RUNS.store(0, Ordering::Relaxed);
    let thread = task::kthread::spawn_kernel_thread("idle-soak", pinger, PriorityClass::Normal)
        .map_err(|error| format!("spawn: {error}"))?;
    let result = soak_rounds(thread);
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::finish(thread, 0);
    THREAD_QUEUE.notify_all();
    let reaped = task::reap_child().map(|(slot, _)| slot);
    task::harness::reset();
    result?;
    check!(
        reaped == Some(thread),
        "the soak thread was not reaped: {reaped:?}"
    );
    Ok(())
}

/// Sleep `ns`: the kernel task outranks the thread (`Interactive` over
/// `Normal`), so only its sleeps let the thread run.
fn nap_ns(ns: u64) {
    task::wait_sleep_ns(crate::arch::clock::monotonic_ns() + ns);
}

/// Sleep until the thread is parked on its queue, at most a second.
fn until_parked(thread: usize) -> Result<(), String> {
    for _ in 0..5_000 {
        if THREAD_QUEUE.contains(thread) {
            return Ok(());
        }
        nap_ns(200_000);
    }
    Err(format!("the soak thread {thread} never parked"))
}

fn soak_rounds(thread: usize) -> Result<(), String> {
    until_parked(thread)?;
    let loops = idle::probe::loops();
    for round in 0..SOAK_ROUNDS {
        let runs = THREAD_RUNS.load(Ordering::Relaxed);
        THREAD_QUEUE.notify_all();
        nap_ns(200_000);
        // The thread had its turn during the sleep and parks again.
        until_parked(thread)?;
        check!(
            THREAD_RUNS.load(Ordering::Relaxed) > runs,
            "round {round}: the thread never ran"
        );
    }
    let idled = idle::probe::loops() - loops;
    crate::trace::test!(
        "smp_idle_switch_soak:INFO:rounds={SOAK_ROUNDS} idle_passes={idled} thread_runs={}",
        THREAD_RUNS.load(Ordering::Relaxed)
    );
    check!(
        idled >= SOAK_ROUNDS / 2,
        "only {idled} idle passes in {SOAK_ROUNDS} rounds"
    );
    Ok(())
}

/// The cases that need the real entry lock (`LAZYOS_SMP=1`).
#[cfg(lazyos_smp)]
mod locked;
