//! Freeze forensics (issue #861), the scheduler's side: `SCHED:STUCK` and
//! `LOCK:WAIT`, judged on the lines they print and on what they must not say.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::forensics_suite::{captured, cleanup, fork, fresh};
use super::*;
use crate::arch::clock::monotonic_ns;
use crate::arch::irqoff::set_task_syscall_for_test;
use crate::task::relax::YieldMutex;
use crate::task::wait::WaitQueue;
use crate::task::yieldlock::{simulate_holder_for_test, simulate_wait_for_test};
use crate::task::{lockwait, stuck, PriorityClass, TaskState, WaitKind};

fn asleep() -> TaskState {
    TaskState::Blocked {
        wait: WaitKind::Sleep,
        deadline: None,
    }
}

/// A runnable task whose kernel frame is its only frame, after two seconds,
/// once, then every ten; a blocked one or one in user mode never.
fn stuck_reports_a_runnable_kernel_task() -> Result<(), String> {
    fresh()?;
    let spinner = fork()?;
    let sleeper = fork()?;
    let user = fork()?;
    task::harness::set_kernel_frame(spinner);
    task::harness::set_kernel_frame(sleeper);
    task::harness::set_state(sleeper, asleep());
    stuck::sample_at_for_test(1000);
    check!(
        stuck::watched_for_test(spinner).is_some(),
        "a runnable kernel task was not watched"
    );
    check!(
        stuck::watched_for_test(sleeper).is_none() && stuck::watched_for_test(user).is_none(),
        "a blocked or user-mode task was watched"
    );
    let early = captured(|| stuck::sample_at_for_test(1199));
    check!(early.is_empty(), "reported before two seconds: {early:?}");
    let text = captured(|| stuck::sample_at_for_test(1200));
    check!(
        text.contains(&format!("SCHED:STUCK tgid={spinner} thread={spinner}/")),
        "no report for {spinner} in {text:?}"
    );
    check!(text.contains("krip=0x"), "no kernel rip: {text:?}");
    check!(
        !text.contains("state="),
        "a constant field is back: {text:?}"
    );
    check!(text.matches("SCHED:STUCK").count() == 1, "{text:?}");
    let quiet = captured(|| stuck::sample_at_for_test(1900));
    check!(quiet.is_empty(), "repeated within ten seconds: {quiet:?}");
    let again = captured(|| stuck::sample_at_for_test(2200));
    check!(again.contains("SCHED:STUCK"), "no repeat after ten seconds");
    // It returns to user mode: forgotten.
    task::harness::set_frame_cs(spinner, 0x23);
    stuck::sample_at_for_test(2300);
    check!(
        stuck::watched_for_test(spinner).is_none(),
        "a task back in user mode is still watched"
    );
    cleanup();
    Ok(())
}

/// A task inside a syscall is keyed by it: the report says which, counts the
/// time from the syscall's entry, names the user `rip` it came from, and a
/// new invocation of the same call starts the watch over.
fn stuck_is_keyed_by_the_syscall() -> Result<(), String> {
    fresh()?;
    let spinner = fork()?;
    task::harness_frames::set_preempted_in_irq(spinner, 0x40_1000);
    // A timer tick found it inside `fallocate`, entered at tick 900.
    set_task_syscall_for_test(spinner, Some((false, 285, 900)));
    task::harness_frames::set_syscall_user_rip(spinner, 0x40_0123);
    stuck::sample_at_for_test(1000);
    check!(
        stuck::watched_for_test(spinner).is_some(),
        "a runnable task in a syscall was not watched"
    );
    let text = captured(|| stuck::sample_at_for_test(1200));
    check!(text.contains("abi=linux nr=285"), "wrong call in {text:?}");
    check!(
        text.contains("ms=3000"),
        "the time is not counted from the syscall's entry: {text:?}"
    );
    check!(text.contains("urip=0x400123"), "no user rip: {text:?}");
    // The same call again, a new invocation: the watch starts over.
    set_task_syscall_for_test(spinner, Some((false, 285, 1250)));
    let reset = captured(|| stuck::sample_at_for_test(1300));
    check!(reset.is_empty(), "a new invocation was reported: {reset:?}");
    set_task_syscall_for_test(spinner, None);
    cleanup();
    Ok(())
}

/// A task blocked inside a syscall (a `read` that waits for minutes) is not
/// stuck, and neither is one that was only interrupted in user mode and
/// switched out inside an interrupt stub, however much CPU it uses.
fn stuck_ignores_blocked_and_interrupted_tasks() -> Result<(), String> {
    fresh()?;
    let reader = fork()?;
    let busy = fork()?;
    set_task_syscall_for_test(reader, Some((false, 0, 100)));
    task::harness::set_state(reader, asleep());
    // `busy` runs user code and is preempted by an IRQ whose stub yields: the
    // saved frame is a kernel one, the one the CPU pushed on top is ring 3.
    task::harness_frames::set_preempted_in_irq(busy, 0x40_2000);
    task::set_priority(busy, PriorityClass::Normal);
    let before = task::cpu_ticks(busy);
    for now in (1000..6000u64).step_by(50) {
        // The busy task runs a few ticks (the sampler is the kernel task and
        // never looks at the running one), then the sampler looks.
        task::harness::switch_current(busy);
        for _ in 0..5 {
            task::harness::simulate_tick();
        }
        task::harness::switch_current(task::KERNEL_TASK);
        let text = captured(|| stuck::sample_at_for_test(now));
        check!(text.is_empty(), "tick {now}: reported {text:?}");
    }
    check!(
        task::cpu_ticks(busy) > before,
        "the busy task never ran: the case proves nothing"
    );
    check!(
        stuck::watched_for_test(busy).is_none() && stuck::watched_for_test(reader).is_none(),
        "a blocked or interrupted task is watched"
    );
    set_task_syscall_for_test(reader, None);
    cleanup();
    Ok(())
}

/// Soak: many tasks, each entering and leaving the kernel over many samples;
/// a sample prints at most its row limit and the watch table never leaks.
fn soak_stuck_sampling_is_bounded() -> Result<(), String> {
    fresh()?;
    let mut slots = Vec::new();
    for _ in 0..30 {
        let slot = fork()?;
        task::harness::set_kernel_frame(slot);
        slots.push(slot);
    }
    let mut now = 1000;
    let mut worst = 0;
    for round in 0..120 {
        now += 50;
        let text = captured(|| stuck::sample_at_for_test(now));
        let lines = text.matches("SCHED:STUCK tgid").count();
        worst = worst.max(lines);
        check!(lines <= 8, "round {round}: {lines} lines in one sample");
        if round % 40 == 39 {
            for &slot in &slots {
                task::harness::set_frame_cs(slot, 0x23);
            }
            stuck::sample_at_for_test(now + 1);
            for &slot in &slots {
                check!(
                    stuck::watched_for_test(slot).is_none(),
                    "slot {slot} still watched after returning to user mode"
                );
                task::harness::set_kernel_frame(slot);
            }
        }
    }
    check!(worst > 0, "thirty stuck tasks never reported");
    cleanup();
    Ok(())
}

static LOCK: YieldMutex<u64> = YieldMutex::named("testlock", 0);

/// Taking a lock again and again, uncontended, is never a wait; and a wait
/// that ended (the lock was taken) leaves nothing to report.
fn repeated_acquisition_is_never_reported() -> Result<(), String> {
    fresh()?;
    let me = task::current();
    for round in 0..20_000u64 {
        *LOCK.lock() += round & 1;
    }
    check!(
        crate::task::yieldlock::waiting(me, monotonic_ns()).is_none(),
        "an uncontended lock left a wait behind"
    );
    check!(
        LOCK.info_for_test().holder().is_none(),
        "the lock still names a holder after its guard dropped"
    );
    {
        let _guard = LOCK.lock();
        check!(
            LOCK.info_for_test().holder() == Some(me),
            "the holder is {:?}, not {me}",
            LOCK.info_for_test().holder()
        );
    }
    let reported = lockwait::sample_at_for_test(monotonic_ns() + 3_000_000_000);
    check!(reported.is_empty(), "reported {reported:?}");
    cleanup();
    Ok(())
}

/// A long wait is reported with the lock's name and its holder, once per five
/// seconds; a short wait and a finished one are not.
fn lock_wait_reports_a_long_wait() -> Result<(), String> {
    fresh()?;
    let slow = fork()?;
    let brief = fork()?;
    let holder = fork()?;
    const S: u64 = 1_000_000_000;
    let now = 100 * S;
    simulate_holder_for_test(&LOCK, Some(holder));
    simulate_wait_for_test(slow, Some((&LOCK, now - 600_000_000)));
    simulate_wait_for_test(brief, Some((&LOCK, now - 100_000_000)));
    let mut reported = Vec::new();
    let text = captured(|| reported = lockwait::sample_at_for_test(now));
    check!(reported == [slow], "reported {reported:?}, wanted [{slow}]");
    check!(
        text.contains("LOCK:WAIT lock=testlock holder="),
        "no named lock in {text:?}"
    );
    check!(
        text.contains(&format!("holder={holder}/")),
        "the holder is not named in {text:?}"
    );
    check!(text.contains("waiters=2"), "no waiter count in {text:?}");
    simulate_wait_for_test::<u64>(brief, None);
    check!(
        lockwait::sample_at_for_test(now + S).is_empty(),
        "the same wait was reported within five seconds"
    );
    check!(
        lockwait::sample_at_for_test(now + 6 * S) == [slow],
        "a wait lasting six seconds was not reported again"
    );
    // The wait ends: the lock is taken.
    simulate_wait_for_test::<u64>(slow, None);
    simulate_wait_for_test::<u64>(brief, None);
    simulate_holder_for_test(&LOCK, None);
    check!(
        lockwait::sample_at_for_test(now + 60 * S).is_empty(),
        "a finished wait was reported"
    );
    cleanup();
    Ok(())
}

// ---- a real contender --------------------------------------------------

static HELD: AtomicBool = AtomicBool::new(false);
static DONE: AtomicUsize = AtomicUsize::new(0);
static HOLDER_SLOT: AtomicUsize = AtomicUsize::new(0);
static NAPS: WaitQueue = WaitQueue::new(WaitKind::Sleep);
static PARKED: WaitQueue = WaitQueue::new(WaitKind::Sleep);

fn finish_thread() -> ! {
    DONE.fetch_add(1, Ordering::Relaxed);
    let me = task::current();
    loop {
        x86_64::instructions::interrupts::disable();
        PARKED.wait_ns(me, None);
    }
}

/// Take the lock and park inside it, as a VFS holder waits for the disk.
extern "C" fn holder() -> ! {
    {
        let mut work = LOCK.lock();
        HOLDER_SLOT.store(task::current(), Ordering::Relaxed);
        HELD.store(true, Ordering::Release);
        NAPS.wait_ns(task::current(), Some(monotonic_ns() + 900_000_000));
        *work += 1;
    }
    finish_thread()
}

extern "C" fn contender() -> ! {
    while !HELD.load(Ordering::Acquire) {
        NAPS.wait_ns(task::current(), Some(monotonic_ns() + 50_000));
    }
    *LOCK.lock() += 1;
    finish_thread()
}

/// A contender that really waits for a held lock is named, with the lock and
/// its holder; once it has the lock nothing remains.
fn real_contention_is_reported() -> Result<(), String> {
    use crate::arch::irqchip;
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
    lockwait::reset_for_test();
    HELD.store(false, Ordering::Relaxed);
    DONE.store(0, Ordering::Relaxed);
    let holder_slot =
        task::kthread::spawn_kernel_thread("lock-holder", holder, PriorityClass::Normal)
            .map_err(|error| format!("spawn: {error}"))?;
    let contender_slot =
        task::kthread::spawn_kernel_thread("lock-waiter", contender, PriorityClass::Normal)
            .map_err(|error| format!("spawn: {error}"))?;
    let saved: [bool; 16] = core::array::from_fn(|l| irqchip::is_masked(l as u8));
    for line in 0..16u8 {
        irqchip::set_masked(line, line != 0);
    }
    task::relax::set_test_park(true);
    task::set_priority(task::KERNEL_TASK, PriorityClass::Realtime);
    x86_64::instructions::interrupts::enable();
    let start = monotonic_ns();
    while monotonic_ns() - start < 700_000_000 {
        task::idle_ns(monotonic_ns() + 200_000);
    }
    x86_64::instructions::interrupts::disable();
    let now = monotonic_ns();
    let mut reported = Vec::new();
    let text = captured(|| reported = lockwait::sample_at_for_test(now));
    x86_64::instructions::interrupts::enable();
    // Let it finish.
    while DONE.load(Ordering::Relaxed) < 2 && monotonic_ns() - start < 3_000_000_000 {
        task::idle_ns(monotonic_ns() + 200_000);
    }
    x86_64::instructions::interrupts::disable();
    let after = crate::task::yieldlock::waiting(contender_slot, monotonic_ns());
    task::set_priority(task::KERNEL_TASK, PriorityClass::Interactive);
    task::relax::set_test_park(false);
    for (line, masked) in saved.into_iter().enumerate() {
        irqchip::set_masked(line as u8, masked);
    }
    let done = DONE.load(Ordering::Relaxed);
    task::harness::switch_current(task::KERNEL_TASK);
    for slot in [holder_slot, contender_slot] {
        task::harness::finish(slot, 0);
    }
    PARKED.notify_all();
    NAPS.notify_all();
    while task::reap_child().is_some() {}
    task::harness::reset();
    // SAFETY: both threads are finished and never run again.
    if LOCK.is_locked() {
        unsafe { LOCK.force_unlock() };
    }
    check!(done == 2, "{done} of 2 threads finished");
    check!(
        reported == [contender_slot],
        "reported {reported:?} not [{contender_slot}]"
    );
    check!(
        text.contains("LOCK:WAIT lock=testlock")
            && text.contains(&format!("holder={holder_slot}/")),
        "the lock and its holder are not named in {text:?}"
    );
    check!(
        after.is_none(),
        "a wait is still recorded after the lock was taken"
    );
    check!(
        HOLDER_SLOT.load(Ordering::Relaxed) == holder_slot,
        "the holder was not who the test thinks"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "forensics_stuck_reports_a_runnable_kernel_task",
        stuck_reports_a_runnable_kernel_task,
    ),
    (
        "forensics_stuck_is_keyed_by_the_syscall",
        stuck_is_keyed_by_the_syscall,
    ),
    (
        "forensics_stuck_ignores_blocked_and_interrupted_tasks",
        stuck_ignores_blocked_and_interrupted_tasks,
    ),
    (
        "forensics_soak_stuck_sampling_is_bounded",
        soak_stuck_sampling_is_bounded,
    ),
    (
        "forensics_lock_repeated_acquisition_is_never_reported",
        repeated_acquisition_is_never_reported,
    ),
    (
        "forensics_lock_wait_reports_a_long_wait",
        lock_wait_reports_a_long_wait,
    ),
    (
        "forensics_lock_real_contention_is_reported",
        real_contention_is_reported,
    ),
];
