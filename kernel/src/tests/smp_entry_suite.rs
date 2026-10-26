//! The kernel entry lock (docs/architecture/smp.md, "The entry lock";
//! `sync::entry`). Correctness: the hold rules a gate applies, mutual
//! exclusion and first-come-first-served turns between CPUs, and a waiter
//! that still serves the shootdowns sent to it while it spins with
//! interrupts off. Stress: every online CPU taking and dropping the lock in
//! a loop around a deliberately non-atomic update.
//!
//! The suite runs on the boot CPU holding the lock (the boot's hold), so a
//! case that lets the APs in drops it first and takes it back last. The AP
//! jobs run inside their reschedule IPI, interrupts off, exactly the state
//! a gate takes the lock in. The cross-CPU cases are vacuous with one CPU
//! online; in a kernel built without `LAZYOS_SMP=1` the lock is compiled out
//! and one case checks that it is.
//!
//! One case holds in either kernel: no tick may find the task table held by
//! the context it interrupted (`smp_entry_tick_never_finds_table_held`).

use super::*;

#[cfg(not(lazyos_smp))]
pub(super) const CASES: &[(&str, Test)] = &[
    ("smp_entry_lock_compiled_out", compiled_out),
    (
        "smp_entry_tick_never_finds_table_held",
        tick_never_finds_table_held,
    ),
];

/// Soak with the real PIT, as the kernel task's `PERF:` reports run: task
/// snapshots with `IF=1`. A tick that lands while one holds the task table
/// spins in `schedule` on it forever, holding the entry lock (the SMP perf
/// desktop boot hang: `entry lock: CPU2 waited 30 s; held by CPU0`, CPU0 in
/// `schedule` on the table its own `PERF:sched` report held). The test
/// harness counts and skips such a tick instead, so this fails, not hangs.
fn tick_never_finds_table_held() -> Result<(), String> {
    use super::preempt_lock_suite::{
        kernel_task_only, with_irqs_on, SOAK_MAX_ROUNDS, SOAK_TICKS, TIMER_LINE,
    };
    use crate::task::{harness, introspect::TaskSnapshot};
    kernel_task_only();
    let _ = harness::take_tick_lock_stats();
    let _ = harness::take_table_holder_ticks();
    let start = task::ticks();
    let mut rounds = 0u64;
    with_irqs_on(Some(TIMER_LINE), || {
        while task::ticks() < start + SOAK_TICKS && rounds < SOAK_MAX_ROUNDS {
            core::hint::black_box(TaskSnapshot::snapshot());
            rounds += 1;
        }
        Ok(())
    })?;
    let (in_kernel, _) = harness::take_tick_lock_stats();
    let held = harness::take_table_holder_ticks();
    let elapsed = task::ticks() - start;
    check!(
        elapsed >= SOAK_TICKS,
        "only {elapsed} ticks in {rounds} rounds: the PIT did not run"
    );
    check!(
        in_kernel >= SOAK_TICKS / 2,
        "only {in_kernel} of {elapsed} ticks interrupted the snapshots"
    );
    check!(
        held == 0,
        "{held} of {in_kernel} ticks found the task table held ({rounds} snapshots)"
    );
    Ok(())
}

/// Without `LAZYOS_SMP=1` the lock is an empty inline, and every CPU "holds"
/// it.
#[cfg(not(lazyos_smp))]
fn compiled_out() -> Result<(), String> {
    check!(crate::sync::entry::held_here(), "a stub that is not held");
    crate::sync::entry::enter(3, 0);
    crate::sync::entry::leave(3);
    Ok(())
}

#[cfg(lazyos_smp)]
pub(super) use imp::CASES;

#[cfg(lazyos_smp)]
mod imp {
    use super::super::smp_tlb_suite::online_aps;
    use super::*;
    use crate::arch::{percpu, smp};
    use crate::sync::entry;
    use alloc::vec::Vec;
    use core::arch::x86_64::_rdtsc;
    use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    pub(in crate::tests) const CASES: &[(&str, Test)] = &[
        ("smp_entry_lock_hold_rules", hold_rules),
        ("smp_entry_lock_excludes_every_cpu", excludes_every_cpu),
        (
            "smp_entry_lock_waiter_serves_shootdowns",
            waiter_serves_shootdowns,
        ),
        ("smp_entry_lock_turns_are_fifo", turns_are_fifo),
        (
            "smp_entry_tick_never_finds_table_held",
            super::tick_never_finds_table_held,
        ),
    ];

    /// Critical sections per CPU in the exclusion case (`--soak` builds
    /// with `LAZYOS_SMP_SOAK=1`: the long one).
    const ROUNDS: u64 = if option_env!("LAZYOS_SMP_SOAK").is_some() {
        200_000
    } else {
        5_000
    };

    /// The exclusion case's shared state: a counter updated with a load, a
    /// pause and a store (a lost update shows a second CPU inside), and the
    /// number of CPUs inside the section right now.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    static INSIDE: AtomicU32 = AtomicU32::new(0);
    static OVERLAPS: AtomicU64 = AtomicU64::new(0);
    /// The order the FIFO case's waiters got the lock in.
    static TURN: AtomicU64 = AtomicU64::new(0);

    /// The online APs, bringing them up first if no earlier case did (the
    /// bring-up is idempotent): a filtered run (`LAZYOS_TEST_FILTER=smp_entry`)
    /// would otherwise find none and pass vacuously.
    fn aps() -> Vec<usize> {
        smp::bring_up_for_test(percpu::MAX_CPUS);
        online_aps()
    }

    /// One critical section of the exclusion case.
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

    /// The AP side of the exclusion case: `rounds` sections under the lock.
    fn job_sections(rounds: u64) -> u64 {
        for _ in 0..rounds {
            entry::acquire_for_test();
            section();
            entry::release_for_test();
        }
        rounds
    }

    /// Wait for the lock, note the turn it came in, drop it.
    fn job_take_turn(_: u64) -> u64 {
        entry::acquire_for_test();
        let turn = TURN.fetch_add(1, Ordering::AcqRel);
        entry::release_for_test();
        turn
    }

    /// Wait (half a calibrated second) until `tickets` are outstanding on
    /// the lock (the holder's and the waiters'): the AP just started has
    /// queued, whatever the host's scheduling of its vCPU. Short enough that
    /// two of them stay well inside a waiter's watchdog.
    fn wait_until_queued(tickets: u32) -> Result<(), String> {
        let bound = crate::arch::clock::cycles_per_tick() * 50;
        // SAFETY: reads the TSC; no memory or privilege effects.
        let start = unsafe { _rdtsc() };
        while entry::outstanding() < tickets {
            // SAFETY: as above.
            if unsafe { _rdtsc() }.wrapping_sub(start) > bound {
                return Err(format!(
                    "{} tickets outstanding, never {tickets}",
                    entry::outstanding()
                ));
            }
            smp::serve_pending();
            core::hint::spin_loop();
        }
        Ok(())
    }

    /// What one queued round needs: the job each AP runs, and the boot
    /// CPU's parts around its release.
    struct Round<'a> {
        job: fn(u64) -> u64,
        arg: u64,
        /// After each AP's start (its index in the list): a wait that it
        /// queued, or nothing.
        after_start: &'a dyn Fn(usize) -> Result<(), String>,
        /// Still holding, every AP started.
        before_release: &'a dyn Fn(),
        /// Not holding, while the APs run.
        unheld: &'a dyn Fn(),
        seconds: u64,
    }

    /// Start `round.job` on each AP of `aps` in order while the boot CPU
    /// holds the lock, then drop the hold, wait for every started job and
    /// take the hold back. That happens on every path, so a failure is a FAIL
    /// and never an AP left queued behind a hold the suite keeps (its
    /// watchdog would panic the run). Returns the results of the APs that
    /// started, in order, and the first error starting them.
    fn run_queued(aps: &[usize], round: &Round) -> (Vec<Option<u64>>, Result<(), String>) {
        let mut started = Vec::new();
        let mut status = Ok(());
        for (i, &ap) in aps.iter().enumerate() {
            match smp::start_on(ap, round.job, round.arg) {
                Some(ticket) => started.push((ap, ticket)),
                None => {
                    status = Err(format!("AP {ap} offline"));
                    break;
                }
            }
            if let Err(error) = (round.after_start)(i) {
                status = Err(error);
                break;
            }
        }
        (round.before_release)();
        entry::release_for_test();
        (round.unheld)();
        let results = started
            .iter()
            .map(|&(ap, ticket)| smp::finish_on(ap, ticket, round.seconds))
            .collect();
        entry::acquire_for_test();
        (results, status)
    }

    /// The rules a gate applies, on the boot CPU: a ring-0 entry and exit
    /// keep the hold, a resume of ring 3 drops it, an entry from ring 3
    /// takes it back (the Linux `syscall` pair too). The hold is the suite's
    /// again afterwards, whatever failed.
    fn hold_rules() -> Result<(), String> {
        check!(entry::held_here(), "the suite runs without the boot's hold");
        let result = hold_rule_steps();
        if !entry::held_here() {
            entry::acquire_for_test();
        }
        result
    }

    fn hold_rule_steps() -> Result<(), String> {
        entry::enter(0, 0);
        check!(entry::held_here(), "a ring-0 entry dropped the hold");
        entry::leave(0);
        check!(entry::held_here(), "a ring-0 exit dropped the hold");
        entry::leave(3);
        check!(!entry::held_here(), "a ring-3 resume kept the hold");
        entry::enter(3, 0);
        check!(entry::held_here(), "a ring-3 entry did not take the hold");
        entry::leave_user();
        check!(!entry::held_here(), "the syscall return kept the hold");
        entry::enter_user();
        check!(
            entry::held_here(),
            "the syscall entry did not take the hold"
        );
        Ok(())
    }

    /// Every online CPU, the boot CPU included, runs [`ROUNDS`] sections
    /// under the lock at once: no update is lost and no two CPUs are ever
    /// inside together.
    fn excludes_every_cpu() -> Result<(), String> {
        let aps = aps();
        if aps.is_empty() {
            return Ok(());
        }
        COUNTER.store(0, Ordering::Relaxed);
        OVERLAPS.store(0, Ordering::Relaxed);
        let (waits_before, _) = entry::contended();
        let (finished, started) = run_queued(
            &aps,
            &Round {
                job: job_sections,
                arg: ROUNDS,
                after_start: &|_| Ok(()),
                before_release: &|| {},
                unheld: &|| {
                    job_sections(ROUNDS);
                },
                seconds: 600,
            },
        );
        started?;
        for (&ap, done) in aps.iter().zip(&finished) {
            check!(*done == Some(ROUNDS), "AP {ap} did not finish: {done:?}");
        }
        let expected = ROUNDS * (aps.len() as u64 + 1);
        let counted = COUNTER.load(Ordering::Relaxed);
        let overlaps = OVERLAPS.load(Ordering::Relaxed);
        let (waits, longest) = entry::contended();
        crate::trace::test!(
            "smp_entry_lock_excludes_every_cpu:INFO:cpus={} sections={expected} waits={} longest={longest}",
            aps.len() + 1,
            waits - waits_before
        );
        check!(
            overlaps == 0,
            "{overlaps} sections overlapped another CPU's"
        );
        check!(
            counted == expected,
            "{counted} of {expected} updates survived"
        );
        check!(
            waits > waits_before,
            "no CPU ever waited: the case raced nothing"
        );
        Ok(())
    }

    /// An AP waits for the lock (interrupts off) while the boot CPU, holding
    /// it, shoots down a page: the shootdown completes (the waiter served
    /// it from its spin), and the AP gets the lock once it is released.
    fn waiter_serves_shootdowns() -> Result<(), String> {
        let Some(&ap) = aps().first() else {
            return Ok(());
        };
        let cpu = percpu::area(ap).ok_or("no area")?;
        let acks = cpu.ipi.sd_ack.load(Ordering::Acquire);
        let (done, started) = run_queued(
            &[ap],
            &Round {
                job: job_take_turn,
                arg: 0,
                // The boot CPU's ticket and the AP's.
                after_start: &|_| wait_until_queued(2),
                // Returns only once every online CPU acknowledged, the
                // waiter too.
                before_release: &|| crate::mem::tlb::shootdown(0xE000_0000, 1),
                unheld: &|| {},
                seconds: 10,
            },
        );
        started?;
        let served = cpu.ipi.sd_ack.load(Ordering::Acquire).wrapping_sub(acks);
        check!(
            served == 1,
            "the waiter acknowledged {served} requests, not 1"
        );
        check!(
            done.first().copied().flatten().is_some(),
            "the waiter never got the lock"
        );
        Ok(())
    }

    /// Rounds of the FIFO case, alternating which AP queues first: an
    /// unfair lock (both waiters spinning, either may win) passes one round
    /// half the time and all of them almost never.
    const FIFO_ROUNDS: usize = 8;

    /// Two APs queue for the lock one after the other while the boot CPU
    /// holds it: the first to queue gets it first, every round.
    fn turns_are_fifo() -> Result<(), String> {
        let aps = aps();
        let [a, b, ..] = aps[..] else {
            return Ok(());
        };
        for round in 0..FIFO_ROUNDS {
            let order = if round % 2 == 0 { [a, b] } else { [b, a] };
            let base = TURN.load(Ordering::Acquire);
            let (turns, started) = run_queued(
                &order,
                &Round {
                    job: job_take_turn,
                    arg: 0,
                    // The boot CPU's ticket, then one per queued AP.
                    after_start: &|i| wait_until_queued(i as u32 + 2),
                    before_release: &|| {},
                    unheld: &|| {},
                    seconds: 10,
                },
            );
            started?;
            check!(
                turns == [Some(base), Some(base + 1)],
                "round {round}: APs {order:?} queued in that order took turns {turns:?} (from {base})"
            );
        }
        Ok(())
    }
}
