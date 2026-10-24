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
];

#[cfg(not(lazyos_smp))]
pub(super) const CASES: &[(&str, Test)] = COMMON;
#[cfg(lazyos_smp)]
pub(super) const CASES: &[(&str, Test)] = &[
    COMMON[0],
    COMMON[1],
    COMMON[2],
    COMMON[3],
    ("smp_idle_halt_drops_the_hold", locked::halt_drops_the_hold),
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
    let result = oncpu_mark_steps();
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::reset();
    result
}

fn oncpu_mark_steps() -> Result<(), String> {
    let cpu = crate::arch::percpu::current().index() as usize;
    let other = cpu + 1;
    let a = task::spawn_fork().map_err(|e| format!("fork: {e}"))?;
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
mod locked {
    use super::super::smp_tlb_suite::online_aps;
    use super::*;
    use crate::arch::{percpu, smp};
    use crate::sync::entry;
    use alloc::vec::Vec;
    use core::arch::x86_64::_rdtsc;
    use core::sync::atomic::AtomicU32;

    /// Halts the boot CPU makes while the APs hammer the lock (`--soak`:
    /// the long run).
    const HALTS: u64 = if option_env!("LAZYOS_SMP_SOAK").is_some() {
        20_000
    } else {
        300
    };

    static GOT_AT: AtomicU64 = AtomicU64::new(0);
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    static INSIDE: AtomicU32 = AtomicU32::new(0);
    static OVERLAPS: AtomicU64 = AtomicU64::new(0);
    /// Set when the APs of the contention case should stop.
    static STOP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

    fn rdtsc() -> u64 {
        // SAFETY: reads the TSC; no memory or privilege effects.
        unsafe { _rdtsc() }
    }

    fn aps() -> Vec<usize> {
        smp::bring_up_for_test(percpu::MAX_CPUS);
        online_aps()
    }

    /// Wait (half a calibrated second) until `tickets` are outstanding.
    fn wait_until_queued(tickets: u32) -> Result<(), String> {
        let bound = crate::arch::clock::cycles_per_tick() * 50;
        let start = rdtsc();
        while entry::outstanding() < tickets {
            if rdtsc().wrapping_sub(start) > bound {
                return Err(format!("{} tickets outstanding", entry::outstanding()));
            }
            smp::serve_pending();
            core::hint::spin_loop();
        }
        Ok(())
    }

    /// An AP's turn: take the lock, note when, drop it.
    fn job_note_turn(_: u64) -> u64 {
        entry::acquire_for_test();
        GOT_AT.store(rdtsc(), Ordering::Release);
        entry::release_for_test();
        1
    }

    /// An AP queues for the lock the boot CPU holds; the boot CPU halts
    /// once. The AP gets the lock inside that halt (between its start and
    /// its end), so the halt dropped the hold; the boot CPU holds again
    /// after, with its halt mark cleared.
    pub(super) fn halt_drops_the_hold() -> Result<(), String> {
        let Some(&ap) = aps().first() else {
            return Ok(());
        };
        fresh();
        GOT_AT.store(0, Ordering::Release);
        let ticket = smp::start_on(ap, job_note_turn, 0).ok_or("AP offline")?;
        let queued = wait_until_queued(2);
        let before = rdtsc();
        // Ended by the next tick; the AP's turn falls inside it.
        entry::halt();
        let after = rdtsc();
        let held = entry::held_here();
        let marked = percpu::current().entry_halt.load(Ordering::Relaxed);
        if queued.is_err() {
            // Let the AP finish before failing, holding nothing it waits on.
            entry::release_for_test();
            smp::finish_on(ap, ticket, 10);
            entry::acquire_for_test();
        }
        queued?;
        let done = smp::finish_on(ap, ticket, 10);
        check!(held, "the halt returned without the hold");
        check!(!marked, "the halt left its CPU marked");
        check!(done == Some(1), "the AP never got the lock: {done:?}");
        let got = GOT_AT.load(Ordering::Acquire);
        check!(
            got > before && got < after,
            "the AP got the lock at {got}, outside the halt [{before}, {after}]"
        );
        Ok(())
    }

    /// A ring-0 entry into a marked halt takes the lock and clears the
    /// mark (what an interrupt landing on `hlt` does).
    pub(super) fn entry_into_halt() -> Result<(), String> {
        check!(entry::held_here(), "the suite runs without the boot's hold");
        let cpu = percpu::current();
        cpu.entry_halt.store(true, Ordering::Relaxed);
        entry::release_for_test();
        entry::enter(0, 0);
        let held = entry::held_here();
        let marked = cpu.entry_halt.swap(false, Ordering::Relaxed);
        if !held {
            entry::acquire_for_test();
        }
        check!(held, "a ring-0 entry into a halt did not take the lock");
        check!(!marked, "the entry left the halt mark set");
        entry::leave(0);
        check!(entry::held_here(), "a ring-0 resume dropped the hold");
        Ok(())
    }

    fn section() {
        if INSIDE.fetch_add(1, Ordering::AcqRel) != 0 {
            OVERLAPS.fetch_add(1, Ordering::Relaxed);
        }
        let value = COUNTER.load(Ordering::Relaxed);
        for _ in 0..32 {
            core::hint::spin_loop();
        }
        COUNTER.store(value + 1, Ordering::Relaxed);
        INSIDE.fetch_sub(1, Ordering::AcqRel);
    }

    /// Sections under the lock until [`STOP`]; how many.
    fn job_sections_until_stopped(_: u64) -> u64 {
        let mut rounds = 0;
        while !STOP.load(Ordering::Acquire) {
            entry::acquire_for_test();
            section();
            entry::release_for_test();
            rounds += 1;
        }
        rounds
    }

    /// Every AP takes and drops the lock around a section until told to
    /// stop, while the boot CPU, holding it except inside its halts, makes
    /// [`HALTS`] halts with a section before each: naps (a halt in this
    /// task, ended by the tick) and 100 us sleeps (a halt in the idle
    /// context, ended by the deadline timer) in turn. The APs only get in
    /// during those halts, and each interrupt that ends one takes the lock
    /// back against them: no update lost, no overlap, every AP got in.
    pub(super) fn halts_under_contention() -> Result<(), String> {
        let aps = aps();
        if aps.is_empty() {
            return Ok(());
        }
        fresh();
        COUNTER.store(0, Ordering::Relaxed);
        OVERLAPS.store(0, Ordering::Relaxed);
        STOP.store(false, Ordering::Release);
        let mut started = Vec::new();
        for &ap in &aps {
            if let Some(ticket) = smp::start_on(ap, job_sections_until_stopped, 0) {
                started.push((ap, ticket));
            }
        }
        let (waits_before, _) = entry::contended();
        let loops = idle::probe::loops();
        for halt in 0..HALTS {
            section();
            if halt % 2 == 0 {
                task::nap();
            } else {
                super::nap_ns(100_000);
            }
        }
        let idled = idle::probe::loops() - loops;
        STOP.store(true, Ordering::Release);
        // The APs finish their last section once they get the lock.
        entry::release_for_test();
        let finished: Vec<_> = started
            .iter()
            .map(|&(ap, ticket)| smp::finish_on(ap, ticket, 10))
            .collect();
        entry::acquire_for_test();
        let ap_sections: u64 = finished.iter().flatten().sum();
        let expected = ap_sections + HALTS;
        let counted = COUNTER.load(Ordering::Relaxed);
        let (waits, _) = entry::contended();
        crate::trace::test!(
            "smp_idle_halts_under_contention:INFO:aps={} halts={HALTS} idle_passes={idled} sections={expected} waits={}",
            started.len(),
            waits - waits_before
        );
        check!(
            started.len() == aps.len(),
            "only {} APs started",
            started.len()
        );
        check!(
            finished
                .iter()
                .all(|done| done.is_some_and(|rounds| rounds > 0)),
            "an AP never got in or never stopped: {finished:?}"
        );
        check!(OVERLAPS.load(Ordering::Relaxed) == 0, "sections overlapped");
        check!(
            counted == expected,
            "{counted} of {expected} updates survived"
        );
        check!(
            idled >= HALTS / 4,
            "only {idled} idle passes in {} sleeps",
            HALTS / 2
        );
        Ok(())
    }
}
