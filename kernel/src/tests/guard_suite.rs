//! The Interactive-class CPU guard (`task::guard`): a task that stays busy in
//! the top class is demoted for the rest of the window, so services and apps
//! below it keep a share of the CPU; correctness cases and a soak.

use super::*;
use crate::task::guard::{BUDGET_TICKS, WINDOW_TICKS};
use crate::task::{PriorityClass, TaskState};

/// Fresh table with the kernel mux parked (as `sched_suite` does) and a new
/// guard window.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(true);
    check!(
        task::harness::state(task::KERNEL_TASK)
            == Some(TaskState::Blocked {
                wait: task::WaitKind::Sleep,
                deadline: None,
            }),
        "the kernel was not parked for the simulation"
    );
    task::guard::set_enabled(true);
    task::guard::reset_window();
    Ok(())
}

fn child(class: PriorityClass) -> Result<usize, String> {
    let slot = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    check!(
        task::set_priority(slot, class),
        "set_priority({slot}) failed"
    );
    Ok(slot)
}

fn cleanup() {
    for slot in 1..task::MAX_TASKS {
        if task::harness::state(slot).is_some() {
            task::harness::finish(slot, 0);
        }
    }
    task::harness::switch_current(task::KERNEL_TASK);
    while task::reap_child().is_some() {}
    task::set_blocked(false);
    task::harness::reset();
    task::guard::set_enabled(false);
}

/// Run `ticks` simulated ticks and return how many each slot was picked for.
fn picks(ticks: usize) -> Vec<usize> {
    let mut counts = vec![0usize; task::MAX_TASKS];
    for _ in 0..ticks {
        counts[task::harness::simulate_tick()] += 1;
    }
    counts
}

/// An `Interactive` hog and a `Normal` peer: without the guard the peer would
/// get nothing; with it the peer gets the rest of every window.
fn hog_cannot_starve_normal() -> Result<(), String> {
    fresh()?;
    let hog = child(PriorityClass::Interactive)?;
    let peer = child(PriorityClass::Normal)?;
    let windows = 20usize;
    let before = task::guard::demotions();
    let total = windows * WINDOW_TICKS as usize;
    let counts = picks(total);
    // Past the budget the hog competes as Normal: the peer gets half of the
    // rest of each window at least (about a third overall, as its lag earns
    // it the picks). Without the guard it gets none.
    let floor = total / 5;
    check!(
        counts[peer] >= floor,
        "the Normal peer ran {} of {total} ticks, below the guaranteed {floor}",
        counts[peer]
    );
    check!(
        counts[hog] >= total / 2,
        "the hog ran only {} of {total}: the guard over-punishes",
        counts[hog]
    );
    check!(
        task::guard::demotions() > before,
        "no demotion was counted for a task using every tick"
    );
    cleanup();
    Ok(())
}

/// The class comes back at the window's end, so the next window starts with
/// the full budget.
fn class_is_restored_each_window() -> Result<(), String> {
    fresh()?;
    let hog = child(PriorityClass::Interactive)?;
    let _peer = child(PriorityClass::Normal)?;
    picks(BUDGET_TICKS as usize + 2);
    check!(
        task::priority(hog) == Some(PriorityClass::Normal),
        "the hog is {:?} after exceeding the budget",
        task::priority(hog)
    );
    let mut saw_interactive = false;
    for _ in 0..(3 * WINDOW_TICKS) {
        task::harness::simulate_tick();
        saw_interactive |= task::priority(hog) == Some(PriorityClass::Interactive);
    }
    check!(saw_interactive, "the hog never got its class back");
    cleanup();
    Ok(())
}

/// `Interactive` tasks that are busy for a tick or two of a window and then
/// sleep (an input driver, a compositor repainting a small damage) are never
/// demoted, however hard a Normal hog runs behind them.
fn light_interactive_tasks_are_not_demoted() -> Result<(), String> {
    fresh()?;
    let a = child(PriorityClass::Interactive)?;
    let b = child(PriorityClass::Interactive)?;
    let _hog = child(PriorityClass::Normal)?;
    let asleep = TaskState::Blocked {
        wait: task::WaitKind::Sleep,
        deadline: None,
    };
    for window in 0..200 {
        // Each window: both run for a tick or two, then block until the next.
        task::harness::set_state(a, TaskState::Runnable);
        task::harness::set_state(b, TaskState::Runnable);
        for tick in 0..WINDOW_TICKS {
            if tick == 1 {
                task::harness::set_state(a, asleep);
            }
            if tick == 2 {
                task::harness::set_state(b, asleep);
            }
            task::harness::simulate_tick();
            check!(
                task::priority(a) == Some(PriorityClass::Interactive)
                    && task::priority(b) == Some(PriorityClass::Interactive),
                "window {window} tick {tick}: a light task was demoted: {:?} {:?}",
                task::priority(a),
                task::priority(b)
            );
        }
    }
    cleanup();
    Ok(())
}

/// Several `Interactive` tasks that each stay under a third of the CPU but
/// together take all of it are still bounded (the budget is the class's).
fn many_small_interactive_tasks_are_bounded() -> Result<(), String> {
    fresh()?;
    for _ in 0..3 {
        child(PriorityClass::Interactive)?;
    }
    let peer = child(PriorityClass::Normal)?;
    let total = 100 * WINDOW_TICKS as usize;
    let counts = picks(total);
    check!(
        counts[peer] * 100 >= total * 15,
        "the Normal peer got {} of {total} ticks behind three busy Interactive tasks",
        counts[peer]
    );
    cleanup();
    Ok(())
}

/// An explicit class change while demoted sticks: the guard must not hand the
/// old class back at the window's end.
fn explicit_class_beats_pending_restore() -> Result<(), String> {
    fresh()?;
    let hog = child(PriorityClass::Interactive)?;
    let _peer = child(PriorityClass::Normal)?;
    picks(BUDGET_TICKS as usize + 2);
    check!(
        task::priority(hog) == Some(PriorityClass::Normal),
        "not demoted: {:?}",
        task::priority(hog)
    );
    check!(
        task::raise_priority(hog, PriorityClass::Realtime),
        "raise_priority failed"
    );
    picks(3 * WINDOW_TICKS as usize);
    check!(
        task::priority(hog) == Some(PriorityClass::Realtime),
        "a raised task is {:?} after windows turned over",
        task::priority(hog)
    );
    check!(
        task::set_priority(hog, PriorityClass::Background),
        "set_priority failed"
    );
    picks(3 * WINDOW_TICKS as usize);
    check!(
        task::priority(hog) == Some(PriorityClass::Background),
        "a task set to Background is {:?} afterwards",
        task::priority(hog)
    );
    cleanup();
    Ok(())
}

/// A raise to the class a demoted task is going back to changes nothing:
/// `raise_priority` compares against the home class, not Normal.
fn raise_to_home_class_is_a_noop() -> Result<(), String> {
    fresh()?;
    let hog = child(PriorityClass::Interactive)?;
    let _peer = child(PriorityClass::Normal)?;
    picks(BUDGET_TICKS as usize + 2);
    check!(
        task::raise_priority(hog, PriorityClass::Interactive),
        "raise_priority failed"
    );
    check!(
        task::priority(hog) == Some(PriorityClass::Normal),
        "a raise to the home class cancelled the demotion: {:?}",
        task::priority(hog)
    );
    cleanup();
    Ok(())
}

/// A `Normal` task that takes most of every window is demoted below its
/// peers, so a light `Normal` peer is not left with its tiny fair share.
fn normal_hog_yields_to_peers() -> Result<(), String> {
    fresh()?;
    let hog = child(PriorityClass::Normal)?;
    let peer = child(PriorityClass::Normal)?;
    check!(
        task::set_weight(hog, 16) && task::set_weight(peer, 1),
        "set_weight failed"
    );
    let total = 100 * WINDOW_TICKS as usize;
    let before = task::guard::demotions();
    let counts = picks(total);
    // By weight alone the hog would take 16/17 of the CPU and the peer 6 %;
    // past its budget (7 ticks) the hog drops to Background and the peer runs
    // for the other 3 of 10: 30 %.
    check!(
        counts[peer] * 100 >= total * 25,
        "the Normal peer got {} of {total} ticks next to a heavy Normal task",
        counts[peer]
    );
    check!(
        counts[hog] * 100 >= total * 50,
        "the heavy task got only {} of {total}: Background still runs when it can",
        counts[hog]
    );
    check!(
        task::guard::demotions() > before,
        "a task using most of every window was not demoted"
    );
    cleanup();
    Ok(())
}

/// A lone `Normal` hog loses nothing: Background runs when nothing else does.
fn lone_normal_hog_keeps_the_cpu() -> Result<(), String> {
    fresh()?;
    let hog = child(PriorityClass::Normal)?;
    let total = 50 * WINDOW_TICKS as usize;
    let counts = picks(total);
    check!(
        counts[hog] == total,
        "a lone Normal task ran {} of {total} ticks",
        counts[hog]
    );
    cleanup();
    Ok(())
}

/// A configured weight survives a demotion and its restore.
fn weight_survives_a_demotion() -> Result<(), String> {
    fresh()?;
    let hog = child(PriorityClass::Normal)?;
    let _peer = child(PriorityClass::Normal)?;
    check!(task::set_weight(hog, 9), "set_weight failed");
    let mut demoted = false;
    for _ in 0..(3 * WINDOW_TICKS) {
        task::harness::simulate_tick();
        demoted |= task::priority(hog) == Some(PriorityClass::Background);
    }
    check!(demoted, "the hog was never demoted");
    // Whatever tick we stopped on, run to the end of a window and one more
    // tick so the restore has happened.
    let mut restored = false;
    for _ in 0..(2 * WINDOW_TICKS) {
        task::harness::simulate_tick();
        restored |=
            task::priority(hog) == Some(PriorityClass::Normal) && task::weight(hog) == Some(9);
    }
    check!(
        restored,
        "the weight is {:?} (class {:?}) after demotions; it was set to 9",
        task::weight(hog),
        task::priority(hog)
    );
    cleanup();
    Ok(())
}

/// A child created while its parent is demoted starts in the parent's
/// assigned class, not in the demotion the guard is about to undo.
fn child_inherits_assigned_class() -> Result<(), String> {
    fresh()?;
    let parent = child(PriorityClass::Interactive)?;
    let _peer = child(PriorityClass::Normal)?;
    picks(BUDGET_TICKS as usize + 2);
    check!(
        task::priority(parent) == Some(PriorityClass::Normal),
        "the parent was not demoted: {:?}",
        task::priority(parent)
    );
    task::harness::switch_current(parent);
    let forked = task::spawn_fork().map_err(|error| format!("spawn_fork: {error}"))?;
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        task::priority(forked) == Some(PriorityClass::Interactive),
        "a child forked during a demotion started as {:?}",
        task::priority(forked)
    );
    cleanup();
    Ok(())
}

/// Soak: mixed tasks over thousands of windows. After every window a class is
/// its original one or, for an `Interactive` task, Normal (demoted); the
/// Normal tasks keep their share.
fn soak_mixed_classes() -> Result<(), String> {
    fresh()?;
    let wanted = [
        PriorityClass::Interactive,
        PriorityClass::Interactive,
        PriorityClass::Interactive,
        PriorityClass::Normal,
        PriorityClass::Normal,
        PriorityClass::Background,
    ];
    let mut slots = Vec::new();
    for class in wanted {
        slots.push((child(class)?, class));
    }
    let windows = 4000usize;
    let mut ran = vec![0usize; task::MAX_TASKS];
    for window in 0..windows {
        for _ in 0..WINDOW_TICKS {
            ran[task::harness::simulate_tick()] += 1;
        }
        for &(slot, class) in &slots {
            let now = task::priority(slot);
            // Demotion goes down one step per window: an Interactive task
            // needs more than 5 ticks to become Normal, then more than 6
            // Normal ones to become Background, which 10 ticks cannot hold.
            let allowed = now == Some(class)
                || (class == PriorityClass::Interactive && now == Some(PriorityClass::Normal))
                || (class == PriorityClass::Normal && now == Some(PriorityClass::Background));
            check!(
                allowed,
                "window {window}: slot {slot} ({}) is {:?}",
                class.label(),
                now
            );
        }
    }
    let total = windows * WINDOW_TICKS as usize;
    let normal: usize = slots
        .iter()
        .filter(|(_, class)| *class == PriorityClass::Normal)
        .map(|(slot, _)| ran[*slot])
        .sum();
    // Three Interactive hogs over budget leave the Normal class a share of
    // every window: the 5 ticks past the budget go to 5 Normal-class tasks
    // (the demoted hogs compete as peers), 2 of them these tasks, so 20 %.
    check!(
        normal * 100 >= total * 15,
        "the Normal tasks got {normal} of {total} ticks (under 15%)"
    );
    check!(
        task::guard::demotions() > 0,
        "three Interactive hogs were never demoted"
    );
    cleanup();
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "task_guard_hog_cannot_starve_normal",
        hog_cannot_starve_normal,
    ),
    (
        "task_guard_class_is_restored_each_window",
        class_is_restored_each_window,
    ),
    (
        "task_guard_light_interactive_tasks_are_not_demoted",
        light_interactive_tasks_are_not_demoted,
    ),
    (
        "task_guard_many_small_interactive_tasks_are_bounded",
        many_small_interactive_tasks_are_bounded,
    ),
    (
        "task_guard_explicit_class_beats_pending_restore",
        explicit_class_beats_pending_restore,
    ),
    (
        "task_guard_raise_to_home_class_is_a_noop",
        raise_to_home_class_is_a_noop,
    ),
    (
        "task_guard_normal_hog_yields_to_peers",
        normal_hog_yields_to_peers,
    ),
    (
        "task_guard_lone_normal_hog_keeps_the_cpu",
        lone_normal_hog_keeps_the_cpu,
    ),
    (
        "task_guard_weight_survives_a_demotion",
        weight_survives_a_demotion,
    ),
    (
        "task_guard_child_inherits_assigned_class",
        child_inherits_assigned_class,
    ),
    ("task_guard_soak_mixed_classes", soak_mixed_classes),
];
