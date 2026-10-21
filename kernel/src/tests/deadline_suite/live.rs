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
    crate::trace::test!(
        "deadline_event_timer_boot:INFO:tick={} event_timer={} rate={} hypervisor={:?}",
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
#[derive(Clone, Copy)]
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

/// The median lateness a working deadline timer reaches (the plan's exit: a
/// 1 ms sleep returns within 1.2 ms).
const MEDIAN_BOUND_NS: u64 = 200_000;

/// Measurements of one length the strict check takes before it gives up.
///
/// The median of 10 to 50 samples sits at a shared runner's noise floor: a
/// coalesced host timer signal or a vCPU deschedule moves it by 50 us or
/// more (medians of 200-258 us failed the check on unrelated branches,
/// issue #876), so one bad measurement says little about the timer. A
/// deadline timer that is really broken (one that falls back to tick
/// granularity has medians of milliseconds) is late in *every* attempt, so
/// requiring one good attempt out of `ATTEMPTS` removes those failures
/// without letting the regression through. Every attempt is traced and a
/// failure reports all of them.
const ATTEMPTS: usize = 3;

/// Why one measurement of `ns`-long sleeps breaks the strict bounds, if it
/// does.
fn violation(ns: u64, rounds: usize, lateness: &Lateness) -> Option<String> {
    if rounds >= MEDIAN_MIN_ROUNDS && lateness.median > MEDIAN_BOUND_NS {
        return Some(format!(
            "{} us sleeps: median {} us late",
            ns / 1000,
            lateness.median / 1000
        ));
    }
    if lateness.worst > task::NS_PER_TICK {
        return Some(format!(
            "{} us sleeps: worst {} us late",
            ns / 1000,
            lateness.worst / 1000
        ));
    }
    None
}

/// Sleeps from 100 µs to 1 s. Never early anywhere (every sample of every
/// attempt); under hardware acceleration with the deadline timer, each length
/// has an attempt (of up to [`ATTEMPTS`]) whose worst lands within one tick
/// (the host may deschedule the vCPU) and, for every length with at least
/// `MEDIAN_MIN_ROUNDS` rounds, whose median is within 200 µs of the
/// deadline. The 1 s length runs once per attempt to bound the suite's time,
/// so only its worst is judged.
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
            judge_row(ns, rounds, strict, true, || measure(ns, rounds))?;
        }
        Ok(())
    })
}

/// Take up to [`ATTEMPTS`] measurements of one sleep length (`measure_once`)
/// and, when `strict`, require one of them to meet the bounds. A measurement
/// error (a sleep that returned early) is final: the retry is for noise in
/// the lateness, never for the "never early" rule.
fn judge_row(
    ns: u64,
    rounds: usize,
    strict: bool,
    report: bool,
    mut measure_once: impl FnMut() -> Result<Lateness, String>,
) -> Result<(), String> {
    let mut failures = alloc::vec::Vec::new();
    for attempt in 1..=ATTEMPTS {
        let lateness = measure_once()?;
        if report {
            crate::trace::test!(
                "deadline_sleep_accuracy:INFO:{} us x{rounds} (attempt {attempt}): late median {} us, worst {} us",
                ns / 1000,
                lateness.median / 1000,
                lateness.worst / 1000
            );
        }
        match violation(ns, rounds, &lateness).filter(|_| strict) {
            None => return Ok(()),
            Some(why) => failures.push(why),
        }
    }
    Err(format!(
        "none of {ATTEMPTS} attempts met the bounds: {}",
        failures.join("; ")
    ))
}

/// The retry rule itself, on synthetic measurements (it runs everywhere, TCG
/// too, where the real test judges nothing): host noise passes once an
/// attempt is clean, a timer at tick granularity (medians of milliseconds)
/// and a persistent miss fail, and an unaccelerated run is never judged.
pub fn sleep_judge_rules() -> Result<(), String> {
    const US: u64 = 1000;
    let lateness = |median: u64, worst: u64| Lateness {
        worst: worst * US,
        median: median * US,
    };
    let run = |strict: bool, rows: &[Lateness]| {
        let mut left = rows.iter();
        judge_row(1_000_000, 50, strict, false, || {
            left.next()
                .copied()
                .ok_or_else(|| String::from("measured more than expected"))
        })
    };

    check!(run(true, &[lateness(100, 400)]).is_ok(), "a clean row");
    check!(
        run(true, &[lateness(258, 400), lateness(120, 400)]).is_ok(),
        "one noisy median (258 us) was not forgiven"
    );
    check!(
        run(true, &[lateness(120, 11_000), lateness(120, 500)]).is_ok(),
        "one deschedule (worst 11 ms) was not forgiven"
    );
    let tick_granular = [
        lateness(5_000, 9_900),
        lateness(4_800, 9_800),
        lateness(5_200, 9_900),
    ];
    check!(
        run(true, &tick_granular).is_err(),
        "a timer at tick granularity passed"
    );
    check!(
        run(true, &[lateness(250, 400); 3]).is_err(),
        "a median that is always over the bound passed"
    );
    let err = run(true, &[lateness(250, 400); 3]).unwrap_err();
    check!(
        err.contains("250 us"),
        "the failure hides the numbers: {err}"
    );
    check!(
        run(false, &tick_granular[..1]).is_ok(),
        "an unaccelerated run was judged"
    );
    // Few rounds judge only the worst.
    let few = judge_row(1_000_000_000, 1, true, false, || Ok(lateness(900_000, 400)));
    check!(few.is_ok(), "a one-round row was judged by its median");
    Ok(())
}
