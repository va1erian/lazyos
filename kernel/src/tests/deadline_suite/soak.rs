//! Real kernel threads arming and cancelling timers under the deadline
//! timer, while the kernel task wakes some of them early.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::*;
use crate::arch::clock;
use crate::task::timerq::TIMERS;
use crate::task::wait::WaitQueue;
use crate::task::{PriorityClass, WaitKind, WakeReason};

/// Where the threads sleep (the kernel task's notifies cancel those timers).
static SOAK: WaitQueue = WaitQueue::new(WaitKind::Sleep);
/// Where a thread parks for good once it is done.
static PARKED: WaitQueue = WaitQueue::new(WaitKind::Sleep);
static TIMED_OUT: AtomicU64 = AtomicU64::new(0);
static WOKEN: AtomicU64 = AtomicU64::new(0);
static EARLY: AtomicU64 = AtomicU64::new(0);
static DONE: AtomicUsize = AtomicUsize::new(0);
static SEED: AtomicU64 = AtomicU64::new(1);

const THREADS: usize = 8;
const SLEEPS: u64 = 400;
/// The whole soak must finish within this (it needs about 0.2 s).
const LIMIT_NS: u64 = 30_000_000_000;

/// Sleep [`SLEEPS`] times for 20 µs to 2 ms each, counting how each ended.
extern "C" fn sleeper() -> ! {
    let me = task::current();
    let mut rng = Rng(SEED.fetch_add(0x9E37_79B9, Ordering::Relaxed) | 1);
    for _ in 0..SLEEPS {
        let deadline = clock::monotonic_ns() + 20_000 + rng.below(2_000_000);
        x86_64::instructions::interrupts::disable();
        let reason = SOAK.wait_ns(me, Some(deadline));
        let now = clock::monotonic_ns();
        x86_64::instructions::interrupts::enable();
        match reason {
            WakeReason::TimedOut if now < deadline => {
                EARLY.fetch_add(1, Ordering::Relaxed);
            }
            WakeReason::TimedOut => {
                TIMED_OUT.fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                WOKEN.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    DONE.fetch_add(1, Ordering::Relaxed);
    loop {
        x86_64::instructions::interrupts::disable();
        PARKED.wait_ns(me, None);
    }
}

/// Eight threads sleep 400 times each on random sub-tick deadlines while the
/// kernel task wakes one of them early every 300 µs: every sleep ends
/// (timed out or woken), none times out early, and no timer is left queued.
pub fn thread_soak() -> Result<(), String> {
    kernel_only();
    for counter in [&TIMED_OUT, &WOKEN, &EARLY] {
        counter.store(0, Ordering::Relaxed);
    }
    DONE.store(0, Ordering::Relaxed);
    let mut slots = alloc::vec::Vec::new();
    for _ in 0..THREADS {
        let slot = task::kthread::spawn_kernel_thread(
            "deadline-soak",
            sleeper,
            PriorityClass::Interactive,
        )
        .map_err(|e| format!("spawn: {e}"))?;
        slots.push(slot);
    }
    let start = clock::monotonic_ns();
    let finished = with_tick(|| {
        while DONE.load(Ordering::Relaxed) < THREADS {
            if clock::monotonic_ns() - start > LIMIT_NS {
                return false;
            }
            // The queue and task-table locks are taken with interrupts off
            // (#382): a tick landing while the mux holds them deadlocks.
            x86_64::instructions::interrupts::without_interrupts(|| SOAK.notify_one());
            task::idle_ns(clock::monotonic_ns() + 300_000);
        }
        true
    });
    let (timed_out, woken, early) = (
        TIMED_OUT.load(Ordering::Relaxed),
        WOKEN.load(Ordering::Relaxed),
        EARLY.load(Ordering::Relaxed),
    );
    serial_println!(
        "TEST:deadline_thread_soak:INFO:{} ms, {timed_out} timed out, {woken} woken early, {early} timed out early",
        (clock::monotonic_ns() - start) / 1_000_000
    );
    let queued: alloc::vec::Vec<usize> = slots
        .iter()
        .copied()
        .filter(|&slot| TIMERS.lock().deadline_of(slot).is_some())
        .collect();
    for &slot in &slots {
        task::harness::finish(slot, 0);
    }
    SOAK.notify_all();
    PARKED.notify_all();
    while task::reap_child().is_some() {}
    task::harness::reset();
    check!(
        finished,
        "the soak did not finish in {} s",
        LIMIT_NS / 1_000_000_000
    );
    check!(early == 0, "{early} sleeps timed out before their deadline");
    check!(
        timed_out + woken == THREADS as u64 * SLEEPS,
        "{} sleeps ended, expected {}",
        timed_out + woken,
        THREADS as u64 * SLEEPS
    );
    check!(
        timed_out > 0 && woken > 0,
        "the soak never cancelled or never expired"
    );
    check!(queued.is_empty(), "timers left queued for {queued:?}");
    Ok(())
}
