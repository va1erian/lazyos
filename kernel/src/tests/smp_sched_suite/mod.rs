//! The APs schedule (docs/smp-plan.md S8.3c; `arch::smp::sched`,
//! `task::kick`, `task::idle::start_ap`).
//!
//! Correctness: kernel threads run on the APs; a wake the boot CPU will not
//! act on is handed to an idle AP at once (the kick); a task that dies while
//! current on an AP is switched away from there, reaped, and its parent told
//! again; idle ticks fold to the machine's share. Stress: ping-pong pairs
//! handing a token across CPUs, and a churn of short threads that exit on
//! the APs and are reaped on the boot CPU, with every kernel stack back.
//! The partner grace (`task::kick`): kept inside it, placed after it or at
//! the tick, plain wakes never kept, no wake lost or run twice in a soak.
//!
//! The suite's own task (the kernel task) stays on the boot CPU (it is
//! pinned there, `task::kick`). Each case lets the APs schedule and holds
//! them back again before it returns (`task::idle::pause_aps`), so the
//! cases after it find them idle with their tick stopped. In a kernel
//! without `LAZYOS_SMP=1` the APs never schedule, which one case checks.

use super::*;

#[cfg(not(lazyos_smp))]
pub(super) const CASES: &[(&str, Test)] = &[("smp_sched_needs_the_smp_kernel", needs_smp)];

/// Without the entry lock the APs are never let in.
#[cfg(not(lazyos_smp))]
fn needs_smp() -> Result<(), String> {
    check!(
        !crate::arch::smp::start_aps_for_test(),
        "a kernel without LAZYOS_SMP=1 let the APs schedule"
    );
    Ok(())
}

#[cfg(lazyos_smp)]
mod cases;
#[cfg(lazyos_smp)]
mod grace;
#[cfg(lazyos_smp)]
mod rig;
#[cfg(lazyos_smp)]
mod scaling;
#[cfg(lazyos_smp)]
mod stress;

#[cfg(lazyos_smp)]
pub(super) const CASES: &[(&str, Test)] = &[
    ("smp_sched_aps_run_threads", cases::aps_run_threads),
    (
        "smp_sched_kick_wakes_an_idle_ap",
        cases::kick_wakes_an_idle_ap,
    ),
    (
        "smp_sched_remote_death_is_reaped",
        cases::remote_death_is_reaped,
    ),
    ("smp_sched_idle_ticks_fold", cases::idle_ticks_fold),
    ("smp_sched_ap_sleep_is_exact", cases::ap_sleep_is_exact),
    (
        "smp_sched_partner_wake_keeps_its_grace",
        grace::partner_wake_keeps_its_grace,
    ),
    (
        "smp_sched_grace_ends_at_the_tick",
        grace::grace_ends_at_the_tick,
    ),
    ("smp_sched_pingpong_soak", stress::pingpong_soak),
    ("smp_sched_grace_soak", grace::grace_soak),
    ("smp_sched_thread_churn_soak", stress::thread_churn_soak),
    ("smp_sched_ring3_tasks_on_aps", stress::ring3_tasks_on_aps),
    (
        "smp_sched_parallel_counting_scales",
        scaling::parallel_counting_scales,
    ),
];
