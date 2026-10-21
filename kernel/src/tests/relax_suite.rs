//! Contended `YieldMutex`es across scheduling classes (issue #609).
//!
//! A desktop boot hung at a `stat` from the compositor: `xuid` (Interactive)
//! wanted the VFS lock while a `Normal` task held it, parked on the disk.
//! Classes are strict, so a contender that only yielded stayed the best pick
//! forever and the holder, runnable again once its disk wait ended, never got
//! the CPU back to release the lock. Contenders now park (`task::relax`).
//!
//! Both tests run real kernel threads on the real timer: one holder parked
//! inside its critical section against a contender of a higher class, and a
//! soak of every class hammering one lock with parks inside and outside it.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use super::*;
use crate::arch::clock::monotonic_ns;
use crate::task::relax::YieldMutex;
use crate::task::wait::WaitQueue;
use crate::task::{PriorityClass, WaitKind};

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "relax_contender_outranking_holder_lets_it_run",
        contender_outranking_holder_lets_it_run,
    ),
    ("relax_soak_every_class_one_lock", soak_every_class_one_lock),
    (
        "relax_fair_lock_hands_off_to_waiter",
        fair_lock_hands_off_to_waiter,
    ),
    ("relax_fair_soak_every_class", fair_soak_every_class),
];

/// The lock under test and the work done under it.
static LOCK: YieldMutex<u64> = YieldMutex::new(0);
/// A fair lock (the ext2 gate's kind, issue #940) for the hand-off tests.
static FAIR: YieldMutex<u64> = YieldMutex::named_fair("fair", 0);
/// Hand-off test: holds the first thread completed, and the count the
/// waiter saw when it got the lock.
static HOLDS: AtomicU64 = AtomicU64::new(0);
static WAITER_SAW: AtomicU64 = AtomicU64::new(u64::MAX);
/// The holder has taken [`LOCK`] (the contender may now try).
static HELD: AtomicBool = AtomicBool::new(false);
/// Where threads park inside or between critical sections, and for good.
static NAPS: WaitQueue = WaitQueue::new(WaitKind::Sleep);
static PARKED: WaitQueue = WaitQueue::new(WaitKind::Sleep);
/// Threads that finished.
static DONE: AtomicUsize = AtomicUsize::new(0);
/// Per-thread seeds and role numbers, handed out in start order.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// How long the holder stays parked with the lock held: a disk wait.
const HOLD_NS: u64 = 2_000_000;
/// Soak: threads, the classes they get in turn, and rounds per thread.
const SOAK_THREADS: usize = 8;
const CLASSES: [PriorityClass; 4] = [
    PriorityClass::Background,
    PriorityClass::Normal,
    PriorityClass::Interactive,
    PriorityClass::Realtime,
];
const SOAK_ROUNDS: u64 = 300;

/// Park the calling thread (interrupts off) until `ns` from now.
fn nap_ns(ns: u64) {
    NAPS.wait_ns(task::current(), Some(monotonic_ns() + ns));
}

/// A thread's last act: count itself done, then park forever.
fn finish_thread() -> ! {
    DONE.fetch_add(1, Ordering::Relaxed);
    let me = task::current();
    loop {
        x86_64::instructions::interrupts::disable();
        PARKED.wait_ns(me, None);
    }
}

/// Take the lock, then park inside the critical section as a VFS holder
/// waits for the disk.
extern "C" fn holder() -> ! {
    {
        let mut work = LOCK.lock();
        HELD.store(true, Ordering::Release);
        nap_ns(HOLD_NS);
        *work += 1;
    }
    finish_thread()
}

/// Wait (parked) until the holder has the lock, then contend for it.
extern "C" fn contender() -> ! {
    while !HELD.load(Ordering::Acquire) {
        nap_ns(50_000);
    }
    *LOCK.lock() += 1;
    finish_thread()
}

/// One soak thread: rounds of lock, sometimes park inside, bump, unlock,
/// sometimes park outside.
extern "C" fn soak_thread() -> ! {
    soak_on(&LOCK)
}

/// The same soak on the fair lock.
extern "C" fn fair_soak_thread() -> ! {
    soak_on(&FAIR)
}

fn soak_on(lock: &'static YieldMutex<u64>) -> ! {
    let mut seed = NEXT.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed) | 1;
    let mut next = move || {
        seed ^= seed >> 12;
        seed ^= seed << 25;
        seed ^= seed >> 27;
        seed.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    for _ in 0..SOAK_ROUNDS {
        {
            let mut work = lock.lock();
            if next() % 4 == 0 {
                nap_ns(10_000 + next() % 90_000);
            }
            *work += 1;
        }
        if next() % 3 == 0 {
            nap_ns(5_000 + next() % 50_000);
        }
    }
    finish_thread()
}

/// Spawn `(entry, class)` threads, idle the kernel task until all finished or
/// `limit_ns` passed (interrupts on, only the tick unmasked, contenders
/// allowed to park), then end and reap them.
pub(super) fn run(threads: &[(extern "C" fn() -> !, PriorityClass)], limit_ns: u64) -> Result<(), String> {
    use crate::arch::irqchip;
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
    DONE.store(0, Ordering::Relaxed);
    let mut slots = Vec::new();
    for &(entry, class) in threads {
        let slot = task::kthread::spawn_kernel_thread("relax", entry, class)
            .map_err(|error| format!("spawn: {error}"))?;
        slots.push(slot);
    }
    let saved: [bool; 16] = core::array::from_fn(|l| irqchip::is_masked(l as u8));
    for line in 0..16u8 {
        irqchip::set_masked(line, line != 0);
    }
    task::relax::set_test_park(true);
    // The kernel task outranks every thread, so it gets back to its clock
    // even while a regression livelocks them (strict classes).
    task::set_priority(task::KERNEL_TASK, PriorityClass::Realtime);
    let start = monotonic_ns();
    let mut timed_out = false;
    while DONE.load(Ordering::Relaxed) < threads.len() {
        if monotonic_ns() - start > limit_ns {
            timed_out = true;
            break;
        }
        // Returns with interrupts on.
        task::idle_ns(monotonic_ns() + 200_000);
    }
    x86_64::instructions::interrupts::disable();
    task::set_priority(task::KERNEL_TASK, PriorityClass::Interactive);
    task::relax::set_test_park(false);
    for (line, masked) in saved.into_iter().enumerate() {
        irqchip::set_masked(line as u8, masked);
    }
    let done = DONE.load(Ordering::Relaxed);
    task::harness::switch_current(task::KERNEL_TASK);
    for &slot in &slots {
        task::harness::finish(slot, 0);
    }
    PARKED.notify_all();
    NAPS.notify_all();
    while task::reap_child().is_some() {}
    task::harness::reset();
    // A thread finished while spinning on the lock left it unlocked.
    // SAFETY: every thread that could hold the guard is finished and will
    // never run again, so nobody owns the lock.
    for lock in [&LOCK, &FAIR] {
        if lock.is_locked() {
            unsafe { lock.force_unlock() };
        }
    }
    check!(
        !timed_out,
        "{done} of {} threads finished in {} ms",
        threads.len(),
        limit_ns / 1_000_000
    );
    Ok(())
}

/// An Interactive contender against a Normal holder parked inside the lock:
/// the holder must get the CPU back when its park ends and release the lock.
/// Before the fix the contender won every pick and this timed out.
pub fn contender_outranking_holder_lets_it_run() -> Result<(), String> {
    *LOCK.lock() = 0;
    HELD.store(false, Ordering::Relaxed);
    let parks = task::relax::parks();
    let started = monotonic_ns();
    run(
        &[
            (holder, PriorityClass::Normal),
            (contender, PriorityClass::Interactive),
        ],
        2_000_000_000,
    )?;
    let elapsed_ms = (monotonic_ns() - started) / 1_000_000;
    let work = *LOCK.lock();
    let parked = task::relax::parks() - parks;
    crate::trace::test!(
        "relax_contender_outranking_holder_lets_it_run:INFO:ms={elapsed_ms} \
         contender_parks={parked}"
    );
    check!(work == 2, "the lock saw {work} of 2 critical sections");
    check!(parked > 0, "the contender never parked on the held lock");
    Ok(())
}

/// Soak: eight threads, two per class, take one lock 300 times each with
/// parks inside and outside it. Every section completes (none lost, none
/// doubled) and the Background threads finish too: no class starves a
/// holder below it.
pub fn soak_every_class_one_lock() -> Result<(), String> {
    *LOCK.lock() = 0;
    let parks = task::relax::parks();
    let threads: Vec<(extern "C" fn() -> !, PriorityClass)> = (0..SOAK_THREADS)
        .map(|index| (soak_thread as extern "C" fn() -> !, CLASSES[index % 4]))
        .collect();
    let started = monotonic_ns();
    run(&threads, 20_000_000_000)?;
    let elapsed_ms = (monotonic_ns() - started) / 1_000_000;
    let work = *LOCK.lock();
    let parked = task::relax::parks() - parks;
    crate::trace::test!(
        "relax_soak_every_class_one_lock:INFO:sections={work} ms={elapsed_ms} \
         contender_parks={parked}"
    );
    let expected = SOAK_THREADS as u64 * SOAK_ROUNDS;
    check!(
        work == expected,
        "{work} of {expected} critical sections ran"
    );
    check!(parked > 0, "no contender ever parked");
    Ok(())
}

/// Back-to-back long holds: the first thread takes [`FAIR`], parks inside
/// (a slow write), releases and at once asks again, five times. The second
/// arrives during the first hold and must get the lock at that hold's end,
/// not after the releaser's later holds (a plain lock lets the releaser win
/// every race against a parked waiter: `LOCK:WAIT ms=740` on the box).
extern "C" fn repeat_holder() -> ! {
    for _ in 0..5 {
        let mut work = FAIR.lock();
        HELD.store(true, Ordering::Release);
        nap_ns(HOLD_NS);
        *work += 1;
        HOLDS.fetch_add(1, Ordering::AcqRel);
    }
    finish_thread()
}

extern "C" fn fair_waiter() -> ! {
    while !HELD.load(Ordering::Acquire) {
        nap_ns(50_000);
    }
    let mut work = FAIR.lock();
    WAITER_SAW.store(HOLDS.load(Ordering::Acquire), Ordering::Release);
    *work += 100;
    drop(work);
    finish_thread()
}

pub fn fair_lock_hands_off_to_waiter() -> Result<(), String> {
    *FAIR.lock() = 0;
    HELD.store(false, Ordering::Relaxed);
    HOLDS.store(0, Ordering::Relaxed);
    WAITER_SAW.store(u64::MAX, Ordering::Relaxed);
    run(
        &[
            (repeat_holder, PriorityClass::Normal),
            (fair_waiter, PriorityClass::Normal),
        ],
        5_000_000_000,
    )?;
    let saw = WAITER_SAW.load(Ordering::Acquire);
    let work = *FAIR.lock();
    crate::trace::test!("relax_fair_lock_hands_off_to_waiter:INFO:holds_before_waiter={saw}");
    check!(work == 105, "the lock saw {work} of 105");
    check!(saw == 1, "the waiter got the lock after {saw} holds, not 1");
    Ok(())
}

/// Soak: eight threads of every class on a fair lock with parks inside and
/// outside it: every section runs, nobody starves, nothing hangs.
pub fn fair_soak_every_class() -> Result<(), String> {
    *FAIR.lock() = 0;
    let threads: Vec<(extern "C" fn() -> !, PriorityClass)> = (0..SOAK_THREADS)
        .map(|index| (fair_soak_thread as extern "C" fn() -> !, CLASSES[index % 4]))
        .collect();
    run(&threads, 20_000_000_000)?;
    let work = *FAIR.lock();
    let expected = SOAK_THREADS as u64 * SOAK_ROUNDS;
    check!(
        work == expected,
        "{work} of {expected} critical sections ran"
    );
    check!(
        FAIR.info_for_test().waiting_for_test() == 0,
        "waiters left on the fair lock"
    );
    Ok(())
}
