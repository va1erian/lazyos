//! The same-class wake rule and its bound (docs/performance-plan.md P6.2),
//! with real threads on the real timer.
//!
//! Two `Normal` CPU hogs (interrupts on, never parking) and a `Normal` waker
//! that sleeps 200 µs in a loop share the CPU for a second while the kernel
//! task sleeps. Every wake of the waker is a candidate same-class preemption
//! of a hog; the rule must let it through only while it deserves the CPU, so
//! the switch rate stays near the tick rate instead of following the 5 kHz
//! wake rate, both hogs keep an even share, and the waker still runs far
//! more often than once per tick.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::*;

/// Progress of each hog, and the waker's completed sleeps.
static HOG: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static WAKES: AtomicU64 = AtomicU64::new(0);
/// The waker's lateness past its deadline: total and worst (ns).
static LATE_SUM: AtomicU64 = AtomicU64::new(0);
static LATE_MAX: AtomicU64 = AtomicU64::new(0);
/// Set to make the waker park for good (it is then finished and reaped).
static STOP: AtomicBool = AtomicBool::new(false);

/// The waker's sleep.
const SLEEP_NS: u64 = 200_000;
/// The measured window.
const WINDOW_NS: u64 = 1_000_000_000;

fn hog(index: usize) -> ! {
    // Like user code: interrupts on, no lock held, so the tick and the
    // deadline timer's preemption point can take the CPU at any instruction.
    x86_64::instructions::interrupts::enable();
    loop {
        HOG[index].fetch_add(1, Ordering::Relaxed);
        core::hint::spin_loop();
    }
}

extern "C" fn hog_a() -> ! {
    hog(0)
}

extern "C" fn hog_b() -> ! {
    hog(1)
}

extern "C" fn waker() -> ! {
    let me = task::current();
    loop {
        if STOP.load(Ordering::Relaxed) {
            THREAD_QUEUE.wait(me, None);
            continue;
        }
        let deadline = crate::arch::clock::monotonic_ns() + SLEEP_NS;
        task::wait_sleep_ns(deadline);
        let late = crate::arch::clock::monotonic_ns().saturating_sub(deadline);
        LATE_SUM.fetch_add(late, Ordering::Relaxed);
        LATE_MAX.fetch_max(late, Ordering::Relaxed);
        WAKES.fetch_add(1, Ordering::Relaxed);
    }
}

/// A 5 kHz waker against two hogs in its class: bounded switching, fair
/// hogs, a serviced waker.
pub fn same_class_wake_bounded() -> Result<(), String> {
    fresh();
    for counter in HOG.iter().chain([&WAKES, &LATE_SUM, &LATE_MAX]) {
        counter.store(0, Ordering::Relaxed);
    }
    STOP.store(false, Ordering::Relaxed);
    let spawn = |name, entry| {
        task::kthread::spawn_kernel_thread(name, entry, PriorityClass::Normal)
            .map_err(|error| format!("spawn {name}: {error}"))
    };
    let slots = [
        spawn("hog-a", hog_a)?,
        spawn("hog-b", hog_b)?,
        spawn("waker", waker)?,
    ];
    // Let everything start, then measure one window while the kernel task
    // (Interactive, so it preempts the threads when its sleep ends) sleeps.
    task::idle_ns(crate::arch::clock::monotonic_ns() + 50_000_000);
    let hogs = [
        HOG[0].load(Ordering::Relaxed),
        HOG[1].load(Ordering::Relaxed),
    ];
    let wakes = WAKES.load(Ordering::Relaxed);
    let switches = task::context_switches();
    let started = crate::arch::clock::monotonic_ns();
    task::idle_ns(started + WINDOW_NS);
    let elapsed = crate::arch::clock::monotonic_ns() - started;
    let hogs = [
        HOG[0].load(Ordering::Relaxed) - hogs[0],
        HOG[1].load(Ordering::Relaxed) - hogs[1],
    ];
    let wakes = WAKES.load(Ordering::Relaxed) - wakes;
    let switches = task::context_switches() - switches;
    let per_s = |count: u64| count * 1_000_000_000 / elapsed.max(1);
    let mean_late = LATE_SUM.load(Ordering::Relaxed) / WAKES.load(Ordering::Relaxed).max(1);
    serial_println!(
        "TEST:task_preempt_same_class_wake_bounded:INFO:wakes_per_s={} switches_per_s={} \
         hog_a={} hog_b={} late_mean_us={} late_max_us={}",
        per_s(wakes),
        per_s(switches),
        hogs[0],
        hogs[1],
        mean_late / 1000,
        LATE_MAX.load(Ordering::Relaxed) / 1000
    );
    // Stop: the hogs never park, so finish them where they stand; the waker
    // is finished too (a finished task is never selected again).
    STOP.store(true, Ordering::Relaxed);
    task::harness::switch_current(task::KERNEL_TASK);
    for &slot in &slots {
        task::harness::finish(slot, 0);
    }
    THREAD_QUEUE.notify_all();
    for _ in &slots {
        check!(task::reap_child().is_some(), "a thread was not reapable");
    }
    task::harness::reset();

    // The waker deserves the CPU after each of its own quanta at most once
    // per hog quantum: with a 100 Hz tick that is a few hundred switches a
    // second. Every wake preempting would be twice the wake rate.
    check!(
        per_s(switches) < 2_500,
        "{} switches/s with a {}/s waker: same-class wakes are not bounded",
        per_s(switches),
        per_s(wakes)
    );
    // Not starved: the waker is served more often than the tick alone would.
    check!(
        per_s(wakes) >= 100,
        "the waker completed only {}/s sleeps",
        per_s(wakes)
    );
    // Fair: neither hog lost its share to the other.
    let (low, high) = (hogs[0].min(hogs[1]), hogs[0].max(hogs[1]));
    check!(
        low > 0 && high <= low * 3,
        "the hogs progressed {} and {}: unfair",
        hogs[0],
        hogs[1]
    );
    Ok(())
}
