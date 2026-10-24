//! The correctness cases: threads run on the APs, a kick hands an idle AP a
//! woken task, a remote death is reaped, idle ticks fold.

use core::arch::x86_64::_rdtsc;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::rig::*;
use super::*;
use crate::task::wait::WaitQueue;
use crate::task::{idle, oncpu, WaitKind, WakeReason};

/// Rounds each worker of the first case runs.
const WORK_ROUNDS: u64 = 50;
/// Kicks the second case measures.
const KICKS: usize = 50;

/// CPUs the workers ran on (one bit per area index), and rounds done.
static SEEN: AtomicU64 = AtomicU64::new(0);
static ROUNDS: AtomicU64 = AtomicU64::new(0);
static WORKERS_DONE: AtomicUsize = AtomicUsize::new(0);
/// Where the workers and the kicked thread sleep.
static NAPS: WaitQueue = WaitQueue::new(WaitKind::Sleep);
static KICKED: WaitQueue = WaitQueue::new(WaitKind::Sleep);
/// The CPU and the TSC the kicked thread last ran at.
static RAN_CPU: AtomicU64 = AtomicU64::new(u64::MAX);
static RAN_TSC: AtomicU64 = AtomicU64::new(0);
/// The CPU the napping thread of the death case runs on.
static NAPPER_CPU: AtomicU64 = AtomicU64::new(u64::MAX);

fn rdtsc() -> u64 {
    // SAFETY: reads the TSC; no memory or privilege effects.
    unsafe { _rdtsc() }
}

/// A worker: note its CPU, sleep a millisecond, [`WORK_ROUNDS`] times.
extern "C" fn worker() -> ! {
    let me = task::current();
    for _ in 0..WORK_ROUNDS {
        SEEN.fetch_or(1 << cpu(), Ordering::Relaxed);
        ROUNDS.fetch_add(1, Ordering::Relaxed);
        NAPS.wait_ns(me, Some(now() + 1_000_000));
    }
    WORKERS_DONE.fetch_add(1, Ordering::Relaxed);
    exit_thread()
}

/// One worker per CPU, each sleeping and waking [`WORK_ROUNDS`] times
/// while the suite sleeps: they run on the APs (not only the boot CPU), all
/// finish, and all are reaped with their stacks.
pub fn aps_run_threads() -> Result<(), String> {
    with_aps(|aps| {
        SEEN.store(0, Ordering::Relaxed);
        ROUNDS.store(0, Ordering::Relaxed);
        WORKERS_DONE.store(0, Ordering::Relaxed);
        let stacks = task::diag::live_kstacks();
        let count = aps.len() + 1;
        for _ in 0..count {
            spawn("smp-worker", worker)?;
        }
        // Napping keeps the boot CPU on the suite's task, so the workers
        // run on the APs.
        let done = nap_until(20_000_000_000, || {
            WORKERS_DONE.load(Ordering::Relaxed) == count
        });
        let reaped = reap(count, 2_000_000_000);
        let seen = SEEN.load(Ordering::Relaxed);
        crate::trace::test!(
            "smp_sched_aps_run_threads:INFO:cpus={} seen={seen:#x} rounds={}",
            count,
            ROUNDS.load(Ordering::Relaxed)
        );
        check!(
            done,
            "{} of {count} workers finished",
            WORKERS_DONE.load(Ordering::Relaxed)
        );
        check!(reaped == count, "{reaped} of {count} workers reaped");
        check!(
            seen & !1 != 0,
            "no worker ever ran on an AP (seen {seen:#x})"
        );
        check!(
            task::diag::live_kstacks() == stacks,
            "{} kernel stacks left",
            task::diag::live_kstacks() - stacks
        );
        Ok(())
    })
}

/// The kicked thread: park; once woken, note where and when it ran.
extern "C" fn kicked() -> ! {
    let me = task::current();
    loop {
        KICKED.wait_ns(me, None);
        RAN_CPU.store(cpu() as u64, Ordering::Relaxed);
        RAN_TSC.store(rdtsc(), Ordering::Relaxed);
    }
}

/// The suite (Interactive) wakes a parked `Normal` thread and keeps the
/// boot CPU: the wake does not preempt it, so it is handed to an idle AP
/// (`task::kick`), which runs the thread within a tick, every time.
pub fn kick_wakes_an_idle_ap() -> Result<(), String> {
    with_aps(|_| {
        let thread = spawn("smp-kicked", kicked)?;
        check!(
            sleep_until(1_000_000_000, || KICKED.contains(thread)),
            "the kicked thread never parked"
        );
        let tick = crate::arch::clock::cycles_per_tick();
        let mut latencies = alloc::vec::Vec::new();
        for round in 0..KICKS {
            RAN_TSC.store(0, Ordering::Relaxed);
            let sent = rdtsc();
            KICKED.notify_all();
            // Napping keeps the boot CPU on the suite's task (runnable,
            // halted without the entry lock), so the AP can take the lock.
            let deadline = sent + tick * 100;
            while RAN_TSC.load(Ordering::Relaxed) == 0 && rdtsc() < deadline {
                task::nap();
            }
            let ran = RAN_TSC.load(Ordering::Relaxed);
            let on = RAN_CPU.load(Ordering::Relaxed);
            check!(ran != 0, "round {round}: the woken thread never ran");
            check!(on != 0, "round {round}: it ran on the boot CPU");
            latencies.push(ran.saturating_sub(sent));
            check!(
                sleep_until(1_000_000_000, || KICKED.contains(thread)),
                "round {round}: the thread never parked again"
            );
        }
        latencies.sort_unstable();
        let us = |cycles: u64| cycles * 10_000 / tick.max(1);
        let (median, worst) = (latencies[KICKS / 2], latencies[KICKS - 1]);
        crate::trace::test!(
            "smp_sched_kick_wakes_an_idle_ap:INFO:kicks={KICKS} median_us={} worst_us={}",
            us(median),
            us(worst)
        );
        // Without the kick the AP would only find the thread at its next
        // tick: a median under half a tick is the kick at work. The worst
        // one is left loose for a host (TCG) that stalls a vCPU.
        check!(
            median < tick / 2,
            "the median kicked wake took {} us",
            us(median)
        );
        check!(worst < 5 * tick, "a kicked wake took {} us", us(worst));
        task::harness::finish(thread, 0);
        KICKED.notify_all();
        Ok(())
    })
}

/// The napping thread: runnable, halting in a loop on whatever CPU has it.
extern "C" fn napper() -> ! {
    loop {
        NAPPER_CPU.store(cpu() as u64, Ordering::Relaxed);
        task::nap();
    }
}

/// A thread dies (the suite finishes it) while it is current on an AP: a
/// reap at once is refused (its stack is live there), the AP is kicked off
/// it, the parent's `wait4` queue is told again, and the reap succeeds.
pub fn remote_death_is_reaped() -> Result<(), String> {
    with_aps(|_| {
        let thread = spawn("smp-napper", napper)?;
        let on_ap = || oncpu::cpu_of(thread).is_some_and(|cpu| cpu != 0);
        check!(
            nap_until(2_000_000_000, on_ap),
            "the napping thread never ran on an AP (last on CPU{})",
            NAPPER_CPU.load(Ordering::Relaxed)
        );
        // No sleep between the finish and the first reap: the AP cannot
        // switch away before the suite releases the entry lock.
        task::harness::finish(thread, 0);
        let early = task::reap_child();
        let told =
            crate::task::wait::CHILD_EXIT.wait_ns(task::current(), Some(now() + 1_000_000_000));
        let reaped = task::reap_child().map(|(slot, _)| slot);
        check!(early.is_none(), "reaped {early:?} while it ran on an AP");
        check!(
            told == WakeReason::Woken,
            "the parent was not told again: {told:?}"
        );
        check!(reaped == Some(thread), "reaped {reaped:?}, not {thread}");
        check!(
            oncpu::cpu_of(thread).is_none(),
            "the reaped slot is still marked"
        );
        Ok(())
    })
}

/// Sleeps the AP thread takes, its worst lateness (ns) and where it ran.
const SLEEPS: u64 = 100;
static LATEST_NS: AtomicU64 = AtomicU64::new(0);
static SLEPT: AtomicU64 = AtomicU64::new(0);
static SLEEP_CPUS: AtomicU64 = AtomicU64::new(0);

/// Sleep 2 ms [`SLEEPS`] times, noting how late each wake ran.
extern "C" fn sleeper() -> ! {
    let me = task::current();
    for _ in 0..SLEEPS {
        let due = now() + 2_000_000;
        NAPS.wait_ns(me, Some(due));
        LATEST_NS.fetch_max(now().saturating_sub(due), Ordering::Relaxed);
        SLEEP_CPUS.fetch_or(1 << cpu(), Ordering::Relaxed);
        SLEPT.fetch_add(1, Ordering::Relaxed);
    }
    exit_thread()
}

/// A thread sleeping 2 ms at a time on the APs (the suite naps on the boot
/// CPU) wakes well inside a tick: an AP's earlier deadline is forwarded to
/// the boot CPU's one-shot timer (`event_timer::forward`), not left to the
/// next 10 ms tick.
pub fn ap_sleep_is_exact() -> Result<(), String> {
    with_aps(|_| {
        if !crate::arch::event_timer::available() {
            return Ok(());
        }
        LATEST_NS.store(0, Ordering::Relaxed);
        SLEPT.store(0, Ordering::Relaxed);
        SLEEP_CPUS.store(0, Ordering::Relaxed);
        spawn("smp-sleeper", sleeper)?;
        let done = nap_until(10_000_000_000, || SLEPT.load(Ordering::Relaxed) == SLEEPS);
        let reaped = reap(1, 2_000_000_000);
        let (late, cpus) = (
            LATEST_NS.load(Ordering::Relaxed),
            SLEEP_CPUS.load(Ordering::Relaxed),
        );
        crate::trace::test!(
            "smp_sched_ap_sleep_is_exact:INFO:sleeps={SLEEPS} worst_late_us={} cpus={cpus:#x}",
            late / 1000
        );
        check!(
            done && reaped == 1,
            "the sleeper did not finish ({} sleeps)",
            SLEPT.load(Ordering::Relaxed)
        );
        check!(
            cpus & !1 != 0,
            "the sleeper never woke on an AP ({cpus:#x})"
        );
        check!(
            late < 5_000_000,
            "a 2 ms sleep woke {} us late",
            late / 1000
        );
        Ok(())
    })
}

/// With every CPU idle the idle count advances by the machine's share: no
/// faster than the clock, though every CPU ticks.
pub fn idle_ticks_fold() -> Result<(), String> {
    with_aps(|aps| {
        check!(
            idle::scheduling_cpus() == aps.len() + 1,
            "{} CPUs schedule, {} expected",
            idle::scheduling_cpus(),
            aps.len() + 1
        );
        // Against the clock, not `TICKS`: a loaded host delays the boot
        // CPU's ticks (caught up later) while the APs' keep coming, and an
        // AP's tick is only as exact as its APIC timer's calibration.
        let (idle0, at0) = (task::idle_ticks(), now());
        sleep_ns(200_000_000);
        let (idle1, at1) = (task::idle_ticks(), now());
        let (idled, periods) = (idle1 - idle0, (at1 - at0) / 10_000_000);
        crate::trace::test!("smp_sched_idle_ticks_fold:INFO:periods={periods} idle={idled}");
        check!(
            idled * 2 <= periods * 3 + 4,
            "{idled} idle ticks in {periods} periods: not folded to one machine"
        );
        check!(
            idled * 2 >= periods,
            "only {idled} idle ticks in {periods} periods"
        );
        Ok(())
    })
}
