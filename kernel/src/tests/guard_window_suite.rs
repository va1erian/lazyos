//! The CPU guard's window and process rules, second part (issue #860): ticks
//! replayed from a long syscall at every window position, threads created,
//! sleeping and ending in the middle of a window.

use super::guard_suite::{child, cleanup, fresh};
use super::*;
use crate::process;
use crate::task::guard::{self, WINDOW_TICKS};
use crate::task::{PriorityClass, TaskState, WaitKind};

fn asleep() -> TaskState {
    TaskState::Blocked {
        wait: WaitKind::Sleep,
        deadline: None,
    }
}

/// `ticks` window ticks arrive at the next scheduler entry of `slot`, which
/// is `start` ticks into its window.
fn replay(slot: usize, start: u64, ticks: u64) {
    guard::set_position_for_test(start);
    task::harness::switch_current(slot);
    crate::arch::irq_window::add_uncharged_for_test(ticks);
    task::harness::on_entry(false);
    task::harness::switch_current(task::KERNEL_TASK);
}

/// A long syscall's ticks reach the guard whatever the window position and
/// length: a task that ran 10 ticks or more inside one is over its budget
/// afterwards (the replay used to roll windows and forget them), for both
/// classes; a short one is not.
fn long_replays_are_charged() -> Result<(), String> {
    for class in [PriorityClass::Normal, PriorityClass::Interactive] {
        for start in [0u64, 3, 9] {
            for ticks in [4u64, 10, 15, 20, 100] {
                fresh()?;
                let busy = child(class)?;
                let _peer = child(class)?;
                let before = guard::demotions();
                replay(busy, start, ticks);
                let demoted = guard::demotions() > before;
                let budget = match class {
                    PriorityClass::Normal => u64::from(guard::NORMAL_BUDGET_TICKS),
                    _ => guard::BUDGET_TICKS,
                };
                // What fits in the window plus up to a window more must be
                // seen: over the budget in one of them demotes.
                let first = ticks.min(WINDOW_TICKS - start);
                let rest = (ticks - first).min(WINDOW_TICKS - 1);
                let should = first > budget || rest > budget;
                check!(
                    demoted == should,
                    "{class:?}: {ticks} ticks at window position {start}: demoted {demoted}, expected {should}"
                );
                cleanup();
            }
        }
    }
    Ok(())
}

/// A replay leaves the window position inside the window, not rolled over
/// again and again.
fn replay_keeps_the_window_position() -> Result<(), String> {
    fresh()?;
    let busy = child(PriorityClass::Normal)?;
    for start in 0..WINDOW_TICKS {
        for ticks in [1u64, 9, 10, 11, 25, 1000] {
            replay(busy, start, ticks);
            check!(
                guard::position_for_test() < WINDOW_TICKS,
                "{ticks} ticks at {start} left the window at {}",
                guard::position_for_test()
            );
            // The next real tick must find the guard coherent: charging more
            // ticks never panics and a task is never left half demoted.
            task::harness::switch_current(busy);
            task::harness::on_entry(true);
            task::harness::switch_current(task::KERNEL_TASK);
        }
    }
    cleanup();
    Ok(())
}

/// The threads of a process that sleep when it goes over budget are not
/// demoted (they hold no CPU; the process is one demotion), and join it on the
/// first tick after they wake.
fn sleeping_threads_join_when_they_wake() -> Result<(), String> {
    fresh()?;
    let leader = child(PriorityClass::Normal)?;
    task::harness::switch_current(leader);
    let mut threads = vec![leader];
    for _ in 0..3 {
        let thread = task::spawn_thread("sleeper", process::USER_STACK_TOP, 0, 0)
            .map_err(|error| format!("thread: {error}"))?;
        threads.push(thread);
    }
    task::harness::switch_current(task::KERNEL_TASK);
    let (awake, sleepers) = threads.split_at(2);
    for &slot in sleepers {
        task::harness::set_state(slot, asleep());
    }
    let before = guard::demotions();
    // Run until the awake threads are demoted (the process went over).
    let mut guard_ticks = 0;
    while awake
        .iter()
        .any(|&slot| task::priority(slot) != Some(PriorityClass::Background))
    {
        task::harness::simulate_tick();
        guard_ticks += 1;
        check!(guard_ticks < 20, "the busy threads were never demoted");
    }
    check!(
        guard::demotions() - before == 1,
        "the process counted {} demotions, wanted 1",
        guard::demotions() - before
    );
    check!(
        sleepers
            .iter()
            .all(|&slot| task::priority(slot) == Some(PriorityClass::Normal)),
        "a sleeping thread was demoted with the process"
    );
    // One wakes and runs: its first charged tick joins the demotion (counted
    // on the thread, not again on the process; the window may roll at once).
    task::harness::set_state(sleepers[0], TaskState::Runnable);
    for _ in 0..4 {
        task::harness::simulate_tick();
    }
    check!(
        guard::task_demotions_for_test(sleepers[0]) > 0,
        "the woken thread ran on as Normal in a demoted process"
    );
    check!(
        guard::demotions() - before == 1,
        "joining the demotion counted again"
    );
    cleanup();
    Ok(())
}

/// Threads created and ending in the middle of windows do not break the
/// accounting: the single-threaded peer keeps its share however many threads
/// the busy process has, and the ledger forgets the process each window.
fn threads_come_and_go_mid_window() -> Result<(), String> {
    fresh()?;
    let leader = child(PriorityClass::Normal)?;
    let peer = child(PriorityClass::Normal)?;
    let mut threads = vec![leader];
    let mut ran = vec![0usize; task::MAX_TASKS];
    let windows = 60usize;
    for window in 0..windows {
        for tick in 0..WINDOW_TICKS {
            if tick == 3 && window % 3 == 0 && threads.len() < 9 {
                task::harness::switch_current(leader);
                let thread = task::spawn_thread("coming", process::USER_STACK_TOP, 0, 0)
                    .map_err(|error| format!("thread: {error}"))?;
                task::set_priority(thread, PriorityClass::Normal);
                threads.push(thread);
                task::harness::switch_current(task::KERNEL_TASK);
            }
            if tick == 6 && window % 5 == 4 && threads.len() > 2 {
                let thread = threads.pop().unwrap_or(leader);
                task::harness::finish(thread, 0);
            }
            ran[task::harness::simulate_tick()] += 1;
        }
        task::reclaim_pending();
    }
    let total = windows * WINDOW_TICKS as usize;
    check!(
        ran[peer] * 100 >= total * 20,
        "the single-threaded peer got {} of {total} ticks against a process of up to 9 threads",
        ran[peer]
    );
    let process_ticks: usize = threads.iter().map(|&slot| ran[slot]).sum();
    check!(process_ticks > 0, "the busy process never ran");
    cleanup();
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "task_guard_window_long_replays_are_charged",
        long_replays_are_charged,
    ),
    (
        "task_guard_window_replay_keeps_the_window_position",
        replay_keeps_the_window_position,
    ),
    (
        "task_guard_window_sleeping_threads_join_when_they_wake",
        sleeping_threads_join_when_they_wake,
    ),
    (
        "task_guard_window_threads_come_and_go_mid_window",
        threads_come_and_go_mid_window,
    ),
];
