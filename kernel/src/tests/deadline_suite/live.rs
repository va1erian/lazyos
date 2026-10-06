//! Deadlines through the task table, the scheduler and the hardware.

use super::*;
use crate::arch::{clock, event_timer, timer};
use crate::task::timerq::TIMERS;
use crate::task::wait::WaitQueue;
use crate::task::{TaskState, WaitKind, WakeReason};

fn queued(slot: usize) -> Option<u64> {
    TIMERS.lock().deadline_of(slot)
}

fn blocked(slot: usize) -> bool {
    matches!(task::harness::state(slot), Some(TaskState::Blocked { .. }))
}

/// A tick deadline is stored as `ticks * 10 ms` and passes exactly when the
/// tick sweep reaches it: not one nanosecond earlier.
pub fn tick_conversion() -> Result<(), String> {
    kernel_only();
    let me = task::current();
    let queue = WaitQueue::new(WaitKind::Sleep);
    let target = task::ticks() + 10;
    let ns = task::ticks_to_ns(target);
    queue.park(me, Some(target));
    check!(
        task::harness::state(me)
            == Some(TaskState::Blocked {
                wait: WaitKind::Sleep,
                deadline: Some(ns)
            }),
        "state {:?}",
        task::harness::state(me)
    );
    check!(queued(me) == Some(ns), "queued {:?}", queued(me));
    task::harness::expire_deadlines(target - 1);
    check!(blocked(me), "woke a tick early");
    task::harness::expire_deadlines_ns(ns - 1);
    check!(blocked(me), "woke a nanosecond early");
    task::harness::expire_deadlines(target);
    check!(
        task::harness::take_wake_reason(me) == Some(WakeReason::TimedOut),
        "not timed out at its tick"
    );
    check!(queued(me).is_none(), "the expired entry stayed queued");
    check!(
        queue.notify_all() == 0,
        "a timed-out waiter counted as woken"
    );
    Ok(())
}

/// A nanosecond deadline between two ticks expires at that nanosecond.
pub fn ns_expiry_exact() -> Result<(), String> {
    kernel_only();
    let me = task::current();
    let queue = WaitQueue::new(WaitKind::Sleep);
    let deadline = clock::monotonic_ns() + 1_234_567;
    queue.park_ns(me, Some(deadline));
    task::harness::expire_deadlines_ns(deadline - 1);
    check!(blocked(me), "woke before its deadline");
    task::harness::expire_deadlines_ns(deadline);
    check!(
        task::harness::take_wake_reason(me) == Some(WakeReason::TimedOut),
        "not timed out at its deadline"
    );
    queue.notify_all();
    Ok(())
}

/// A wake cancels the timer; an entry left behind by a state change the
/// queue did not see (stale) is dropped at expiry without waking anyone.
pub fn wake_cancels_timer() -> Result<(), String> {
    kernel_only();
    let me = task::current();
    let queue = WaitQueue::new(WaitKind::Sleep);
    let deadline = clock::monotonic_ns() + 5_000_000_000;
    queue.park_ns(me, Some(deadline));
    check!(queued(me) == Some(deadline), "not queued");
    check!(queue.notify_one() == 1, "notify did not wake");
    check!(
        queued(me).is_none(),
        "the woken task's timer is still queued"
    );
    let _ = task::harness::take_wake_reason(me);

    // Stale: queued, then made runnable behind the queue's back.
    queue.park_ns(me, Some(deadline));
    task::harness::set_state(me, TaskState::Runnable);
    check!(queued(me) == Some(deadline), "set_state touched the queue");
    task::harness::expire_deadlines_ns(deadline);
    check!(
        task::harness::state(me) == Some(TaskState::Runnable),
        "state {:?}",
        task::harness::state(me)
    );
    check!(
        task::harness::take_wake_reason(me).is_none(),
        "a stale entry recorded a wake"
    );
    check!(queued(me).is_none(), "the stale entry was not dropped");
    queue.notify_all();

    // A deadline-less block cancels a queued one.
    queue.park_ns(me, Some(deadline));
    task::harness::set_state(
        me,
        TaskState::Blocked {
            wait: WaitKind::Signal,
            deadline: None,
        },
    );
    check!(
        queued(me).is_none(),
        "a block without deadline kept the timer"
    );
    task::harness::set_state(me, TaskState::Runnable);
    queue.notify_all();
    Ok(())
}

/// A deadline already in the past ends the wait on the park's own scheduler
/// entry: no tick, no interrupt needed.
pub fn past_returns_at_once() -> Result<(), String> {
    kernel_only();
    let before = clock::monotonic_ns();
    let reason = task::wait_sleep_ns(before.saturating_sub(1));
    let elapsed = clock::monotonic_ns() - before;
    check!(reason == WakeReason::TimedOut, "reason {reason:?}");
    check!(
        elapsed < task::NS_PER_TICK,
        "a past deadline took {elapsed} ns"
    );
    let reason = task::wait_sleep_ns(0);
    check!(reason == WakeReason::TimedOut, "deadline 0: {reason:?}");
    Ok(())
}

/// Native syscall 34: the clock reads `monotonic_ns`, a past `sleep_until`
/// returns "elapsed", an unknown op is `-EINVAL`.
pub fn native_syscall() -> Result<(), String> {
    kernel_only();
    let before = clock::monotonic_ns();
    let read = process::dispatch_for_test(34, 0, 0, 0);
    let after = clock::monotonic_ns();
    check!(
        before <= read && read <= after,
        "{before} <= {read} <= {after}"
    );
    check!(
        process::dispatch_for_test(34, 1, read, 0) == crate::process::timesys::ELAPSED,
        "a past sleep_until did not report elapsed"
    );
    check!(
        process::dispatch_for_test(34, 9, 0, 0) as i64 == -22,
        "unknown op"
    );
    Ok(())
}

/// An absolute wall-clock instant converts to the monotonic reading the
/// wall clock was built from.
pub fn realtime_conversion() -> Result<(), String> {
    let before = clock::monotonic_ns();
    let (secs, nanos) = crate::wallclock::now_ns();
    let after = clock::monotonic_ns();
    let mono = crate::wallclock::wall_to_monotonic_ns(secs as u64, u64::from(nanos));
    check!(
        before <= mono && mono <= after,
        "{before} <= {mono} <= {after}"
    );
    check!(
        crate::wallclock::wall_to_monotonic_ns(0, 0) == 0,
        "the epoch did not saturate to 0"
    );
    Ok(())
}

/// With the PIT as the tick, boot armed the deadline timer (unless the
/// image was built without it); with the APIC as the tick there is none.
pub fn event_timer_boot() -> Result<(), String> {
    let info = timer::info().ok_or("timer::init never ran")?;
    serial_println!(
        "TEST:deadline_event_timer_boot:INFO:tick={} event_timer={} rate={} hypervisor={:?}",
        if info.lapic { "lapic" } else { "pit" },
        event_timer::available(),
        event_timer::rate(),
        hypervisor().map(|s| alloc::string::String::from_utf8_lossy(&s).into_owned())
    );
    if info.lapic {
        check!(
            !event_timer::available(),
            "the APIC is the tick and the deadline timer"
        );
    } else if option_env!("LAZYOS_EVENT_TIMER") != Some("0") && info.source != "none" {
        check!(
            event_timer::available(),
            "the PIT is the tick but no deadline timer"
        );
    }
    Ok(())
}

/// The tick ABI still means 10 ms: `ticks()` and the monotonic clock agree
/// to within one period, and a tick deadline returns on its tick.
pub fn tick_abi_10ms() -> Result<(), String> {
    kernel_only();
    let (ticks, ns) = (task::ticks(), clock::monotonic_ns());
    check!(
        task::ticks_to_ns(ticks) <= ns && ns < task::ticks_to_ns(ticks + 1),
        "ticks {ticks} vs monotonic {ns}"
    );
    with_tick(|| {
        for k in [1u64, 2, 5] {
            let target = task::ticks() + k;
            let reason = task::idle(target);
            let now = task::ticks();
            check!(reason == WakeReason::TimedOut, "reason {reason:?}");
            check!(now >= target, "woke at tick {now}, before {target}");
            check!(now <= target + 1, "woke at tick {now}, target {target}");
        }
        Ok(())
    })
}

/// Lateness summary of one sleep length.
struct Lateness {
    worst: u64,
    median: u64,
}

/// Sleep `ns` from now, `rounds` times; never early, else how late.
fn measure(ns: u64, rounds: usize) -> Result<Lateness, String> {
    let mut late = alloc::vec::Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let deadline = clock::monotonic_ns() + ns;
        let reason = task::idle_ns(deadline);
        let now = clock::monotonic_ns();
        check!(reason == WakeReason::TimedOut, "reason {reason:?}");
        check!(
            now >= deadline,
            "a {ns} ns sleep returned {} ns early",
            deadline - now
        );
        late.push(now - deadline);
    }
    late.sort_unstable();
    Ok(Lateness {
        worst: late[late.len() - 1],
        median: late[late.len() / 2],
    })
}

/// Fewest rounds whose median the strict check judges. A median of one or
/// two samples is just a sample: one vCPU deschedule by a loaded host (a
/// shared CI runner) fails it, so such a length is judged by its worst only.
const MEDIAN_MIN_ROUNDS: usize = 10;

/// Sleeps from 100 µs to 1 s. Never early anywhere; under hardware
/// acceleration with the deadline timer, the worst lands within one tick
/// (the host may deschedule the vCPU) and, for every length with at least
/// `MEDIAN_MIN_ROUNDS` rounds, the median within 200 µs of the deadline
/// (the plan's exit: a 1 ms sleep returns within 1.2 ms). The 1 s length
/// runs once to bound the suite's time, so only its worst is judged.
pub fn sleep_accuracy() -> Result<(), String> {
    kernel_only();
    let strict = accelerated() && event_timer::available();
    with_tick(|| {
        for (ns, rounds) in [
            (100_000u64, 20usize),
            (250_000, 20),
            (1_000_000, 50),
            (3_300_000, 20),
            (12_500_000, 10),
            (50_000_000, 10),
            (1_000_000_000, 1),
        ] {
            let lateness = measure(ns, rounds)?;
            serial_println!(
                "TEST:deadline_sleep_accuracy:INFO:{} us x{rounds}: late median {} us, worst {} us",
                ns / 1000,
                lateness.median / 1000,
                lateness.worst / 1000
            );
            if strict && rounds >= MEDIAN_MIN_ROUNDS {
                check!(
                    lateness.median <= 200_000,
                    "{} us sleeps: median {} us late",
                    ns / 1000,
                    lateness.median / 1000
                );
            }
            if strict {
                check!(
                    lateness.worst <= task::NS_PER_TICK,
                    "{} us sleeps: worst {} us late",
                    ns / 1000,
                    lateness.worst / 1000
                );
            }
        }
        Ok(())
    })
}
