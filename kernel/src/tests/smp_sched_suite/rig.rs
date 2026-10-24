//! What every case of the suite shares: letting the APs schedule and holding
//! them back again, sleeping, and reaping.

use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use super::super::smp_tlb_suite::online_aps;
use super::*;
use crate::arch::{percpu, smp};
use crate::task::idle;

/// Whether the cases may hold timings to a bound: only under hardware
/// acceleration (TCG's virtual time is not real time) and not in a chaos
/// image (`task::chaos`), whose seeded reschedules are meant to disturb
/// exactly that; otherwise the numbers are reported and the behaviour still
/// checked.
pub fn timing_checked() -> bool {
    super::super::deadline_suite::accelerated() && crate::task::chaos::seed().is_none()
}

/// Calibrated nanoseconds since boot.
pub fn now() -> u64 {
    crate::arch::clock::monotonic_ns()
}

/// Park the suite's task for `ns` (the APs get the entry lock meanwhile).
pub fn sleep_ns(ns: u64) {
    task::wait_sleep_ns(now() + ns);
}

/// Sleep in 1 ms steps until `done` holds or `timeout_ns` passed.
pub fn sleep_until(timeout_ns: u64, done: impl Fn() -> bool) -> bool {
    let deadline = now() + timeout_ns;
    while !done() {
        if now() > deadline {
            return false;
        }
        sleep_ns(1_000_000);
    }
    true
}

/// Nap (the suite's task stays current on the boot CPU, halted without the
/// entry lock) until `done` holds or `timeout_ns` passed. Unlike a sleep,
/// the boot CPU cannot pick up a runnable thread meanwhile, so the work goes
/// to the APs.
pub fn nap_until(timeout_ns: u64, done: impl Fn() -> bool) -> bool {
    let deadline = now() + timeout_ns;
    while !done() {
        if now() > deadline {
            return false;
        }
        task::nap();
    }
    true
}

/// A fresh table with the kernel task current and runnable.
pub fn fresh() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
}

/// The CPU running the caller.
pub fn cpu() -> usize {
    percpu::current().index() as usize
}

/// Bring the APs up (if no earlier case did), let them schedule and wait
/// until every one runs its idle context. An empty list: no AP (the cases
/// pass vacuously at `-smp 1`).
pub fn start() -> Result<Vec<usize>, String> {
    smp::bring_up_for_test(percpu::MAX_CPUS);
    let aps = online_aps();
    if aps.is_empty() {
        return Ok(aps);
    }
    if !smp::start_aps_for_test() {
        return Err("the APs have no tick to schedule with".into());
    }
    idle::pause_aps(false);
    let up = sleep_until(2_000_000_000, || aps.iter().all(|&ap| idle::scheduling(ap)));
    check!(up, "the APs never entered their idle contexts");
    Ok(aps)
}

/// Hold the APs back and wait until each runs its idle context again.
pub fn stop(aps: &[usize]) -> Result<(), String> {
    idle::pause_aps(true);
    let idle_now = |ap: usize| {
        percpu::area(ap)
            .is_some_and(|cpu| idle::is_idle(cpu.current.load(Ordering::Relaxed) as usize))
    };
    let parked = sleep_until(2_000_000_000, || aps.iter().all(|&ap| idle_now(ap)));
    check!(parked, "an AP kept running a task after the pause");
    Ok(())
}

/// Run `body` with the APs scheduling, then hold them back and reset the
/// table whatever `body` returned.
pub fn with_aps(body: impl FnOnce(&[usize]) -> Result<(), String>) -> Result<(), String> {
    fresh();
    let aps = start()?;
    let result = if aps.is_empty() { Ok(()) } else { body(&aps) };
    let stopped = if aps.is_empty() { Ok(()) } else { stop(&aps) };
    if stopped.is_err() {
        // A task may still run on an AP: freeing its slot (and stack) now
        // would pull them from under it. The run fails here instead.
        return result.and(stopped);
    }
    task::harness::switch_current(task::KERNEL_TASK);
    while task::reap_child().is_some() {}
    task::harness::reset();
    result.and(stopped)
}

/// Spawn a kernel thread.
pub fn spawn(name: &'static str, entry: extern "C" fn() -> !) -> Result<usize, String> {
    task::kthread::spawn_kernel_thread(name, entry, task::PriorityClass::Normal)
        .map_err(|error| format!("spawn {name}: {error}"))
}

/// A thread's last act: finished, then off the CPU for good.
pub fn exit_thread() -> ! {
    task::harness::finish(task::current(), 0);
    task::exit_cpu()
}

/// Reap finished children until `count` were reaped or `timeout_ns`
/// passed, napping between tries; how many were.
pub fn reap(count: usize, timeout_ns: u64) -> usize {
    let mut reaped = 0;
    let deadline = now() + timeout_ns;
    while reaped < count && now() < deadline {
        if task::reap_child().is_some() {
            reaped += 1;
        } else {
            task::nap();
        }
    }
    reaped
}
