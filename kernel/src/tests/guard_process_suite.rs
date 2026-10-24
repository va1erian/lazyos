//! The CPU guard's accounting rules (issue #860): the `Normal` budget is the
//! process's, not the thread's; an input-path `Interactive` task is not demoted with
//! a heavy one; a finished task is charged nothing; ticks taken inside
//! interrupt windows reach the guard. Correctness cases and soaks.

use super::guard_suite::{child, cleanup, fresh, picks};
use super::*;
use crate::process;
use crate::task::guard::WINDOW_TICKS;
use crate::task::{PriorityClass, TaskState};

/// A process of `threads` busy threads (the leader included), all `Normal`.
fn process_with_threads(threads: usize) -> Result<Vec<usize>, String> {
    let leader = child(PriorityClass::Normal)?;
    let mut all = vec![leader];
    task::harness::switch_current(leader);
    for _ in 1..threads {
        let thread = task::spawn_thread("busy-thread", process::USER_STACK_TOP, 0, 0)
            .map_err(|error| format!("thread: {error}"))?;
        task::set_priority(thread, PriorityClass::Normal);
        all.push(thread);
    }
    task::harness::switch_current(task::KERNEL_TASK);
    Ok(all)
}

/// Four busy threads of one process against one busy single-threaded task:
/// the threads are demoted together, and the lone task gets the share of the
/// window the process's budget leaves it (about 30%; with a budget per thread
/// it got a fifth).
fn threads_share_one_budget() -> Result<(), String> {
    fresh()?;
    let threads = process_with_threads(4)?;
    let peer = child(PriorityClass::Normal)?;
    let windows = 30usize;
    let mut together = false;
    let mut ran = vec![0usize; task::MAX_TASKS];
    for _ in 0..windows * WINDOW_TICKS as usize {
        ran[task::harness::simulate_tick()] += 1;
        let low = threads
            .iter()
            .filter(|&&slot| task::priority(slot) == Some(PriorityClass::Background))
            .count();
        check!(
            low == 0 || low == threads.len(),
            "{low} of {} threads of one process were demoted: the budget is not the process's",
            threads.len()
        );
        together |= low == threads.len();
    }
    let total = windows * WINDOW_TICKS as usize;
    check!(together, "the busy process was never demoted");
    // The process may run 6 ticks of a window before it is demoted, and
    // inside the class the stride scheduler shares per *thread*: the four
    // threads reach their budget in about a window and a half of their fair
    // share, so the lone task ends near 30% (it got a fifth with a budget per
    // thread). A fair-share-per-process scheduler would make it 50%; this
    // guard bounds a process, it does not equalise processes. The bounds
    // below are that: better than per-thread, no worse than the budget.
    check!(
        (25..=45).contains(&(ran[peer] * 100 / total)),
        "the single-threaded task ran {} of {total} ticks beside four busy threads",
        ran[peer]
    );
    let process_ticks: usize = threads.iter().map(|&slot| ran[slot]).sum();
    check!(
        process_ticks * 100 >= total * 55,
        "the four threads got only {process_ticks} of {total}: over-punished"
    );
    cleanup();
    Ok(())
}

/// Alone on the machine, a many-threaded process loses nothing to its demotion.
fn lone_process_keeps_the_cpu() -> Result<(), String> {
    fresh()?;
    let threads = process_with_threads(4)?;
    let total = 20 * WINDOW_TICKS as usize;
    let counts = picks(total);
    let ran: usize = threads.iter().map(|&slot| counts[slot]).sum();
    check!(ran == total, "the process ran {ran} of {total} ticks alone");
    cleanup();
    Ok(())
}

/// A heavy `Interactive` task spends the class budget; an input task (a tick
/// or two a window) keeps its class, the heavy one does not. An unmarked
/// light task is demoted with it, as before.
fn input_task_survives_a_heavy_one() -> Result<(), String> {
    fresh()?;
    let heavy = child(PriorityClass::Interactive)?;
    let input = child(PriorityClass::Interactive)?;
    let plain = child(PriorityClass::Interactive)?;
    let _peer = child(PriorityClass::Normal)?;
    task::guard::mark_input(input);
    let asleep = TaskState::Blocked {
        wait: task::WaitKind::Sleep,
        deadline: None,
    };
    let (mut heavy_demoted, mut plain_demoted) = (0, 0);
    for window in 0..100 {
        // Both wake late in the window, after the heavy task spent the budget.
        task::harness::set_state(input, asleep);
        task::harness::set_state(plain, asleep);
        for tick in 0..WINDOW_TICKS {
            if tick == 6 {
                task::harness::set_state(input, TaskState::Runnable);
                task::harness::set_state(plain, TaskState::Runnable);
            }
            if tick == 9 {
                task::harness::set_state(input, asleep);
                task::harness::set_state(plain, asleep);
            }
            task::harness::simulate_tick();
            check!(
                task::priority(input) == Some(PriorityClass::Interactive),
                "window {window} tick {tick}: the input task was demoted with the heavy one"
            );
            heavy_demoted += usize::from(task::priority(heavy) == Some(PriorityClass::Normal));
            plain_demoted += usize::from(task::priority(plain) == Some(PriorityClass::Normal));
        }
    }
    check!(heavy_demoted > 0, "the heavy task was never demoted");
    check!(
        plain_demoted > 0,
        "an unmarked task was never demoted: the exemption is not bounded"
    );
    cleanup();
    Ok(())
}

/// The input share is bounded: busy input tasks are charged like any other
/// once past their share and the pool, so they are demoted too.
fn input_share_is_bounded() -> Result<(), String> {
    fresh()?;
    let mut inputs = Vec::new();
    for _ in 0..3 {
        let slot = child(PriorityClass::Interactive)?;
        task::guard::mark_input(slot);
        inputs.push(slot);
    }
    let _peer = child(PriorityClass::Normal)?;
    let mut demoted = 0;
    for _ in 0..50 * WINDOW_TICKS {
        task::harness::simulate_tick();
        demoted += inputs
            .iter()
            .filter(|&&slot| task::priority(slot) == Some(PriorityClass::Normal))
            .count();
    }
    check!(demoted > 0, "three busy input tasks were never demoted");
    cleanup();
    Ok(())
}

/// A finished task that is still `cur` (the idle fallback) is an idle tick:
/// no CPU time, no quota, no guard charge.
fn done_current_books_an_idle_tick() -> Result<(), String> {
    fresh()?;
    let dead = child(PriorityClass::Normal)?;
    task::harness::finish(dead, 0);
    task::harness::switch_current(dead);
    let ticks_before = task::cpu_ticks(dead);
    let idle_before = task::idle_ticks();
    // Simulated ticks are not time that passed: give the idle count back,
    // or `sysinfo` would see more idle ticks than uptime.
    let idle_after = |booked: u64| {
        task::set_idle_ticks(task::idle_ticks().saturating_sub(booked));
    };
    for _ in 0..40 {
        task::harness::on_entry(true);
    }
    check!(
        task::cpu_ticks(dead) == ticks_before,
        "a finished task accrued {} ticks",
        task::cpu_ticks(dead) - ticks_before
    );
    let booked = task::idle_ticks() - idle_before;
    idle_after(booked);
    check!(booked == 40, "{booked} idle ticks booked, wanted 40");
    check!(
        task::priority(dead) == Some(PriorityClass::Normal),
        "a finished task was demoted"
    );
    task::harness::switch_current(task::KERNEL_TASK);
    cleanup();
    Ok(())
}

/// Ticks taken in interrupt windows count for the guard: a task that spent a
/// long syscall running is over its budget when the scheduler next looks.
fn window_ticks_reach_the_guard() -> Result<(), String> {
    fresh()?;
    let busy = child(PriorityClass::Normal)?;
    let _peer = child(PriorityClass::Normal)?;
    task::harness::switch_current(busy);
    let before = task::guard::demotions();
    crate::arch::irq_window::add_uncharged_for_test(WINDOW_TICKS - 2);
    task::harness::on_entry(false);
    check!(
        task::guard::demotions() > before,
        "{} ticks in windows left the guard untouched",
        WINDOW_TICKS - 2
    );
    check!(
        task::priority(busy) == Some(PriorityClass::Background),
        "the task is {:?} after a long syscall",
        task::priority(busy)
    );
    task::harness::switch_current(task::KERNEL_TASK);
    cleanup();
    Ok(())
}

/// Soak: random mixes of processes with different thread counts, light and
/// heavy `Interactive` tasks and finished tasks, over many windows. The
/// per-process rule never splits a process, shares add up, and nothing
/// finished is ever charged.
fn soak_mixed_processes() -> Result<(), String> {
    fresh()?;
    let big = process_with_threads(5)?;
    let small = process_with_threads(2)?;
    let single = child(PriorityClass::Normal)?;
    let heavy = child(PriorityClass::Interactive)?;
    let light = child(PriorityClass::Interactive)?;
    task::guard::mark_input(light);
    let dead = child(PriorityClass::Normal)?;
    task::harness::finish(dead, 0);
    let asleep = TaskState::Blocked {
        wait: task::WaitKind::Sleep,
        deadline: None,
    };
    let dead_before = task::cpu_ticks(dead);
    let mut ran = vec![0usize; task::MAX_TASKS];
    let windows = 200;
    for window in 0..windows {
        task::harness::set_state(light, TaskState::Runnable);
        for tick in 0..WINDOW_TICKS {
            if tick == 1 {
                task::harness::set_state(light, asleep);
            }
            ran[task::harness::simulate_tick()] += 1;
            for process in [&big, &small] {
                let low = process
                    .iter()
                    .filter(|&&slot| task::priority(slot) == Some(PriorityClass::Background))
                    .count();
                check!(
                    low == 0 || low == process.len(),
                    "window {window}: {low} of {} threads demoted",
                    process.len()
                );
            }
        }
    }
    let total = windows * WINDOW_TICKS as usize;
    let sum = |slots: &[usize]| slots.iter().map(|&slot| ran[slot]).sum::<usize>();
    // Per-process accounting: the single task gets at least what one thread
    // of the five-thread process gets, and the five threads together do not
    // get five times that.
    check!(
        ran[single] * big.len() >= sum(&big),
        "the single task ran {} ticks, the five-thread process {} ({} a thread)",
        ran[single],
        sum(&big),
        sum(&big) / big.len()
    );
    check!(
        sum(&big) + sum(&small) + ran[single] + ran[heavy] + ran[light] >= total * 95 / 100,
        "ticks were lost: the tasks ran {} of {total}",
        sum(&big) + sum(&small) + ran[single] + ran[heavy] + ran[light]
    );
    check!(
        task::cpu_ticks(dead) == dead_before,
        "a finished task was charged {} ticks",
        task::cpu_ticks(dead) - dead_before
    );
    cleanup();
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "task_guard_process_threads_share_one_budget",
        threads_share_one_budget,
    ),
    (
        "task_guard_process_lone_process_keeps_the_cpu",
        lone_process_keeps_the_cpu,
    ),
    (
        "task_guard_process_input_task_survives_a_heavy_one",
        input_task_survives_a_heavy_one,
    ),
    (
        "task_guard_process_input_share_is_bounded",
        input_share_is_bounded,
    ),
    (
        "task_guard_process_done_current_books_an_idle_tick",
        done_current_books_an_idle_tick,
    ),
    (
        "task_guard_process_window_ticks_reach_the_guard",
        window_ticks_reach_the_guard,
    ),
    (
        "task_guard_process_soak_mixed_processes",
        soak_mixed_processes,
    ),
];
