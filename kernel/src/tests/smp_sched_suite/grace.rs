//! The partner grace (`task::kick`): a Messenger partner's wake is kept at
//! a syscall return for a partner about to park, placed once the grace is
//! over or at the next tick when the waker stays out of the kernel, and run
//! where the waker parks when it parks within the grace; any other wake is
//! placed at once.

use core::sync::atomic::{AtomicU64, Ordering};

use super::rig::*;
use super::*;
use crate::task::kick::{self, GRACE_NS};
use crate::task::wait::WaitQueue;
use crate::task::WaitKind;

/// Wakes each correctness case makes.
const ROUNDS: u64 = 30;
/// Wakes the soak makes (`--soak`: more).
const SOAK: u64 = if option_env!("LAZYOS_SMP_SOAK").is_some() {
    20_000
} else {
    1_500
};

static QUEUE: WaitQueue = WaitQueue::new(WaitKind::Sleep);
/// Runs of the woken thread, and the CPUs they were on (one bit each).
static RUNS: AtomicU64 = AtomicU64::new(0);
static CPUS: AtomicU64 = AtomicU64::new(0);
/// Runs that were on the boot CPU, where the suite parks.
static LOCAL: AtomicU64 = AtomicU64::new(0);

/// Park; once woken, count the run and where it was.
extern "C" fn woken() -> ! {
    let me = task::current();
    loop {
        QUEUE.wait_ns(me, None);
        let on = cpu();
        CPUS.fetch_or(1 << on, Ordering::Relaxed);
        if on == 0 {
            LOCAL.fetch_add(1, Ordering::Relaxed);
        }
        RUNS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Start the woken thread and wait for it to park.
fn start_woken() -> Result<usize, String> {
    RUNS.store(0, Ordering::Relaxed);
    CPUS.store(0, Ordering::Relaxed);
    LOCAL.store(0, Ordering::Relaxed);
    // A miss earlier in the run may have cost the suite its grace.
    kick::forgive(task::current());
    let thread = spawn("smp-graced", woken)?;
    check!(
        sleep_until(1_000_000_000, || QUEUE.contains(thread)),
        "the woken thread never parked"
    );
    Ok(thread)
}

/// End the woken thread (the case's table reset reaps it).
fn end_woken(thread: usize) {
    task::harness::finish(thread, 0);
    QUEUE.notify_all();
}

/// Wake the thread as a Messenger partner would. The suite runs in the
/// Interactive class and the thread in Normal, so the wake is placed, not
/// taken here.
fn partner_wake() {
    kick::partner_wakes(|| QUEUE.notify_all());
}

/// Wait (napping, so an AP can take the entry lock) for run `run` and for
/// the thread to park again.
fn await_run(thread: usize, run: u64) -> Result<(), String> {
    check!(
        nap_until(1_000_000_000, || RUNS.load(Ordering::Relaxed) >= run),
        "wake {run}: the thread never ran"
    );
    check!(
        sleep_until(1_000_000_000, || QUEUE.contains(thread)),
        "wake {run}: the thread never parked again"
    );
    Ok(())
}

/// Spin, interrupts off, until a grace begun before `since` is over.
fn outlast_grace(since: u64) {
    while now().saturating_sub(since) <= GRACE_NS {
        core::hint::spin_loop();
    }
}

/// A syscall return inside the grace keeps a partner wake; one after the
/// grace places it, and an AP runs it. A plain wake is placed at once.
pub fn partner_wake_keeps_its_grace() -> Result<(), String> {
    with_aps(|_| {
        let thread = start_woken()?;
        let mut kept = 0;
        for round in 1..=ROUNDS {
            let since = now();
            partner_wake();
            // The grace starts inside the wake, between these two clocks.
            let woke = now();
            check!(kick::remembered_here(), "wake {round} was not remembered");
            kick::flush_after_syscall();
            let fast = now().saturating_sub(since) < GRACE_NS;
            if fast && kick::remembered_here() {
                kept += 1;
            }
            outlast_grace(woke);
            kick::flush_after_syscall();
            check!(
                !kick::remembered_here(),
                "wake {round}: a return after the grace kept it"
            );
            await_run(thread, round)?;
        }
        // A wake that is not a partner's has no grace.
        QUEUE.notify_all();
        kick::flush_after_syscall();
        let plain_kept = kick::remembered_here();
        await_run(thread, ROUNDS + 1)?;
        let cpus = CPUS.load(Ordering::Relaxed);
        crate::trace::test!(
            "smp_sched_partner_wake_keeps_its_grace:INFO:rounds={ROUNDS} kept={kept} cpus={cpus:#x}"
        );
        check!(!plain_kept, "a syscall return kept a plain wake");
        check!(cpus & !1 != 0, "no placed wake ran on an AP ({cpus:#x})");
        if timing_checked() {
            check!(kept * 2 > ROUNDS, "only {kept} of {ROUNDS} wakes were kept");
        }
        end_woken(thread);
        Ok(())
    })
}

/// A waker that goes on without entering the kernel again (interrupts on,
/// no syscall): its CPU's next tick places what the grace kept, and the
/// miss denies the waker the grace (its next wake is placed at once) until
/// the penalty is lifted.
pub fn grace_ends_at_the_tick() -> Result<(), String> {
    with_aps(|_| {
        let thread = start_woken()?;
        let me = task::current();
        let tick_ns = crate::task::NS_PER_TICK;
        let mut worst = 0;
        let mut denied = 0;
        for round in 1..=ROUNDS {
            kick::forgive(me);
            let since = now();
            partner_wake();
            kick::flush_after_syscall();
            x86_64::instructions::interrupts::enable();
            while kick::remembered_here() && now() - since < 20 * tick_ns {
                core::hint::spin_loop();
            }
            x86_64::instructions::interrupts::disable();
            let waited = now() - since;
            worst = worst.max(waited);
            check!(
                !kick::remembered_here(),
                "wake {round}: still kept after 20 ticks"
            );
            await_run(thread, 2 * round - 1)?;
            // A wait past the miss bound costs the waker its grace.
            partner_wake();
            kick::flush_after_syscall();
            let kept = kick::remembered_here();
            if waited > kick::MISS_NS {
                check!(!kept, "wake {round}: a waker that missed kept its grace");
                denied += 1;
            }
            task::nap();
            await_run(thread, 2 * round)?;
        }
        kick::forgive(me);
        crate::trace::test!(
            "smp_sched_grace_ends_at_the_tick:INFO:rounds={ROUNDS} worst_us={} denied={denied}",
            worst / 1000
        );
        if timing_checked() {
            check!(
                worst < 3 * tick_ns,
                "a kept wake waited {} us for the tick",
                worst / 1000
            );
            check!(denied > 0, "no round waited past the miss bound");
        }
        end_woken(thread);
        Ok(())
    })
}

/// [`SOAK`] partner wakes, each ended one of three ways in turn: the waker
/// parks inside the grace (the thread may run right here), naps (placed at
/// once) or returns after the grace (placed then). No wake is lost or run
/// twice.
pub fn grace_soak() -> Result<(), String> {
    with_aps(|_| {
        let thread = start_woken()?;
        let started = now();
        for round in 1..=SOAK {
            partner_wake();
            let woke = now();
            kick::flush_after_syscall();
            match round % 3 {
                0 => sleep_ns(100_000),
                1 => task::nap(),
                _ => {
                    outlast_grace(woke);
                    kick::flush_after_syscall();
                }
            }
            await_run(thread, round)?;
            check!(
                RUNS.load(Ordering::Relaxed) == round,
                "wake {round} ran {} times in all",
                RUNS.load(Ordering::Relaxed)
            );
        }
        let (local, cpus) = (LOCAL.load(Ordering::Relaxed), CPUS.load(Ordering::Relaxed));
        crate::trace::test!(
            "smp_sched_grace_soak:INFO:wakes={SOAK} local={local} cpus={cpus:#x} ms={}",
            (now() - started) / 1_000_000
        );
        check!(cpus & !1 != 0, "no wake ran on an AP ({cpus:#x})");
        if timing_checked() {
            // Every third wake's waker parks inside the grace: most of
            // those run here, not on an AP (an AP's tick may get one first).
            check!(
                local * 2 >= SOAK / 3,
                "only {local} of {} parked wakes ran on the waker's CPU",
                SOAK / 3
            );
        }
        end_woken(thread);
        Ok(())
    })
}
