//! Priority classes and fair-share scheduling (issue #58).

use super::*;
use crate::task::{PriorityClass, TaskState, MAX_WEIGHT, MIN_WEIGHT};

/// Fresh table: kernel registered (Interactive) with zeroed accounting.
/// The kernel is parked so the simulations below exercise the user
/// scheduler alone; [`cleanup`] wakes it again.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        task::priority(task::KERNEL_TASK) == Some(PriorityClass::Interactive),
        "the kernel mux is not Interactive: {:?}",
        task::priority(task::KERNEL_TASK)
    );
    task::set_blocked(true);
    check!(
        task::harness::state(task::KERNEL_TASK)
            == Some(TaskState::Blocked {
                wait: task::WaitKind::Sleep,
                deadline: None,
            }),
        "the kernel was not parked for the simulation: {:?}",
        task::harness::state(task::KERNEL_TASK)
    );
    Ok(())
}

/// Spawn a fork child (which inherits the kernel's class) and put it in
/// `class`.
fn child(class: PriorityClass) -> Result<usize, String> {
    let slot = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    check!(
        task::set_priority(slot, class),
        "set_priority({slot}) failed"
    );
    check!(
        task::priority(slot) == Some(class),
        "slot {slot} did not take class {}",
        class.label()
    );
    Ok(slot)
}

/// Simulate `ticks` timer decisions; returns the picked slot per tick and
/// each task's charged CPU ticks.
fn simulate(ticks: usize) -> (Vec<usize>, Vec<(usize, u64)>) {
    let mut picks = Vec::new();
    for _ in 0..ticks {
        picks.push(task::harness::simulate_tick());
    }
    let usage = task::cpu_usage()
        .into_iter()
        .map(|row| (row.slot, row.ticks))
        .collect();
    (picks, usage)
}

fn runs(picks: &[usize], slot: usize) -> usize {
    picks.iter().filter(|&&pick| pick == slot).count()
}

/// Count the ticks charged to `slot` in a [`simulate`] report.
fn charged(usage: &[(usize, u64)], slot: usize) -> u64 {
    usage
        .iter()
        .find(|(index, _)| *index == slot)
        .map(|(_, ticks)| *ticks)
        .unwrap_or(0)
}

/// Finish every live task, wake the kernel and reset the table.
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
}

/// A `Background` CPU hog cannot starve an `Interactive` task: class beats
/// weight, so the interactive task runs every tick even against the
/// heaviest background hog. Once it blocks, the background task gets the
/// CPU, proving it was only preempted, not starved.
pub fn strict_classes_no_starvation() -> Result<(), String> {
    fresh()?;
    let interactive = child(PriorityClass::Interactive)?;
    let background = child(PriorityClass::Background)?;
    check!(
        task::set_weight(background, MAX_WEIGHT) && task::set_weight(interactive, MIN_WEIGHT),
        "set_weight failed on a live task"
    );
    check!(
        task::weight(background) == Some(MAX_WEIGHT)
            && task::weight(interactive) == Some(MIN_WEIGHT),
        "weights are {:?}/{:?}",
        task::weight(interactive),
        task::weight(background)
    );

    let ticks = 200usize;
    let (picks, usage) = simulate(ticks);
    check!(
        runs(&picks, interactive) == ticks,
        "Interactive ran {} of {ticks} ticks",
        runs(&picks, interactive)
    );
    check!(
        runs(&picks, background) == 0,
        "Background ran {} ticks while Interactive was runnable",
        runs(&picks, background)
    );
    check!(
        charged(&usage, interactive) >= ticks as u64 - 1 && charged(&usage, background) == 0,
        "CPU ticks leaked to the wrong class: {:?}",
        usage
    );

    // The background task is not starved forever: the moment the
    // interactive task blocks, it runs.
    let queue = task::wait::WaitQueue::new(task::WaitKind::Sleep);
    queue.park(interactive, None);
    let (picks, _) = simulate(20);
    check!(
        runs(&picks, background) == 20,
        "Background inherited the CPU only {} of 20 ticks after Interactive blocked",
        runs(&picks, background)
    );
    cleanup();
    Ok(())
}

/// Within one class, weights set the share: a weight-4 and a weight-1
/// `Normal` task split the simulated ticks 4:1 because their strides are
/// inverse to their weights.
pub fn weighted_share_within_class() -> Result<(), String> {
    fresh()?;
    let heavy = child(PriorityClass::Normal)?;
    let light = child(PriorityClass::Normal)?;
    check!(
        task::set_weight(heavy, 4) && task::set_weight(light, 1),
        "set_weight failed on a live task"
    );

    let ticks = 2500usize;
    task::harness::switch_current(heavy);
    let (picks, usage) = simulate(ticks);
    let heavy_runs = runs(&picks, heavy);
    let light_runs = runs(&picks, light);
    check!(
        heavy_runs + light_runs == ticks,
        "the class only used {}/{ticks} ticks",
        heavy_runs + light_runs
    );
    let expected = ticks * 4 / 5;
    check!(
        heavy_runs.abs_diff(expected) <= 2,
        "weight-4 task ran {heavy_runs} times, expected ~{expected}"
    );
    check!(
        light_runs.abs_diff(ticks - expected) <= 2,
        "weight-1 task ran {light_runs} times, expected ~{}",
        ticks - expected
    );
    // The same 4:1 split shows up in the CPU accounting (within one tick
    // of the pick counts, because the first tick charges the start task).
    let heavy_ticks = charged(&usage, heavy);
    let light_ticks = charged(&usage, light);
    check!(
        heavy_ticks + light_ticks == ticks as u64,
        "charged {}+{} ticks for {ticks} decisions",
        heavy_ticks,
        light_ticks
    );
    check!(
        heavy_ticks.abs_diff(heavy_runs as u64) <= 1
            && light_ticks.abs_diff(light_runs as u64) <= 1,
        "CPU accounting ({heavy_ticks}/{light_ticks}) disagrees with selections ({heavy_runs}/{light_runs})"
    );
    cleanup();
    Ok(())
}

/// Blocked and done tasks are never selected; the kernel is the pick only
/// while no user task can run, and a wake puts the user task back first.
pub fn skips_blocked_and_done() -> Result<(), String> {
    fresh()?;
    let interactive = child(PriorityClass::Interactive)?;
    let background = child(PriorityClass::Background)?;

    let queue = task::wait::WaitQueue::new(task::WaitKind::Sleep);
    queue.park(interactive, None);
    task::harness::finish(background, 0);
    check!(
        matches!(
            task::harness::state(interactive),
            Some(TaskState::Blocked { .. })
        ),
        "the interactive task is not blocked: {:?}",
        task::harness::state(interactive)
    );
    check!(
        task::harness::state(background) == Some(TaskState::Done),
        "the background task is not done"
    );
    // Wake the parked kernel: with both user tasks parked/done it is the
    // only candidate left.
    task::wake_task(task::KERNEL_TASK);
    check!(
        task::harness::next_runnable() == task::KERNEL_TASK,
        "the kernel was not chosen when every user task is parked/done"
    );

    let (picks, _) = simulate(10);
    check!(
        picks.iter().all(|&slot| slot == task::KERNEL_TASK),
        "a parked or done task was selected: {picks:?}"
    );

    // A wake makes the user task selectable again: with the mux parked
    // (as it is between frames) the woken task is the only candidate and
    // never loses a tick to a parked or done slot.
    task::set_blocked(true);
    check!(
        queue.notify_one() == 1,
        "notify_one did not wake the interactive task"
    );
    check!(
        task::harness::next_runnable() == interactive,
        "woken task {interactive} is not the next pick"
    );
    let (picks, _) = simulate(5);
    check!(
        picks.iter().all(|&slot| slot == interactive),
        "the woken task did not get the CPU: {picks:?}"
    );
    cleanup();
    Ok(())
}

/// The priority API and CPU accounting: defaults by kind, class changes
/// reset the weight, `set_weight` clamps, and `cpu_usage` reports the
/// ticks charged by the timer path.
pub fn priority_api_and_cpu_accounting() -> Result<(), String> {
    fresh()?;
    let slot = child(PriorityClass::Normal)?;
    check!(
        task::weight(slot) == Some(PriorityClass::Normal.default_weight()),
        "a new Normal task has weight {:?}",
        task::weight(slot)
    );

    check!(
        task::set_weight(slot, u16::MAX),
        "set_weight failed on a live task"
    );
    check!(
        task::weight(slot) == Some(MAX_WEIGHT),
        "weight was not clamped: {:?}",
        task::weight(slot)
    );
    check!(
        task::set_priority(slot, PriorityClass::Realtime),
        "set_priority failed on a live task"
    );
    check!(
        task::weight(slot) == Some(PriorityClass::Realtime.default_weight()),
        "set_priority did not reset the weight: {:?}",
        task::weight(slot)
    );

    // Empty and out-of-range slots report None and reject writes.
    check!(
        task::priority(task::MAX_TASKS + 7).is_none() && task::priority(0).is_some(),
        "priority mishandled an invalid or empty slot"
    );
    check!(
        !task::set_priority(task::MAX_TASKS + 7, PriorityClass::Normal)
            && !task::set_weight(task::MAX_TASKS + 7, 1),
        "the priority API accepted an invalid slot"
    );

    // Simulate ticks with the child as the only runnable user task: it is
    // selected throughout and the charged ticks land in its row.
    task::harness::switch_current(slot);
    let ticks = 100usize;
    let (picks, usage) = simulate(ticks);
    check!(
        picks.iter().all(|&pick| pick == slot),
        "the only runnable user task was not selected: {picks:?}"
    );
    let charged_total: u64 = usage.iter().map(|(_, ticks)| *ticks).sum();
    check!(
        charged_total == ticks as u64,
        "charged {charged_total} ticks for {ticks} decisions: {usage:?}"
    );
    check!(
        task::cpu_ticks(slot) == charged(&usage, slot) && task::cpu_ticks(task::KERNEL_TASK) == 0,
        "cpu_ticks disagrees with cpu_usage: {:?}",
        usage
    );

    let row = task::cpu_usage()
        .into_iter()
        .find(|row| row.slot == slot)
        .ok_or("the child is missing from cpu_usage")?;
    check!(
        row.class == PriorityClass::Realtime
            && row.name == "fork"
            && row.state == TaskState::Runnable
            && row.ticks == charged(&usage, slot),
        "cpu_usage row is {row:?}"
    );
    cleanup();
    Ok(())
}

/// A task that wakes, or is forked, while nothing else is runnable joins at
/// the virtual time the busy tasks reached, not at its old pass: it cannot
/// then hold the CPU against them for a backlog of quanta. (A shell's child
/// that spun on a yielding lock once starved `netd` and an FTP daemon for
/// the 10 s of a FUSE deadline that way.)
pub fn idle_wake_keeps_virtual_time() -> Result<(), String> {
    fresh()?;
    let busy = child(PriorityClass::Normal)?;
    let sleeper = child(PriorityClass::Normal)?;
    let naps = task::wait::WaitQueue::new(task::WaitKind::Sleep);
    let rests = task::wait::WaitQueue::new(task::WaitKind::Sleep);
    naps.park(sleeper, None);
    let ticks = 1000usize;
    let (picks, _) = simulate(ticks);
    check!(
        runs(&picks, busy) == ticks,
        "the busy task ran {} of {ticks} ticks alone",
        runs(&picks, busy)
    );

    // Everyone parks: the CPU is idle when the sleeper wakes and when a task
    // is forked.
    rests.park(busy, None);
    task::harness::switch_current(task::KERNEL_TASK);
    check!(naps.notify_one() == 1, "the sleeper did not wake");
    let forked = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    // A fork of the kernel task inherits its class; the share below is
    // within one class.
    check!(
        task::set_priority(forked, PriorityClass::Normal),
        "set_priority({forked}) failed"
    );
    let stride = 1024 / u64::from(PriorityClass::Normal.default_weight());
    let busy_pass = task::harness::pass(busy).ok_or("busy task vanished")?;
    for (name, slot) in [("woken sleeper", sleeper), ("forked task", forked)] {
        let pass = task::harness::pass(slot).ok_or("task vanished")?;
        check!(
            pass + 2 * stride >= busy_pass,
            "the {name} joined at pass {pass}, {} strides behind the busy task's {busy_pass}",
            (busy_pass - pass) / stride
        );
    }

    // Back together, they share: the busy task is not shut out.
    check!(rests.notify_one() == 1, "the busy task did not wake");
    let (picks, _) = simulate(30);
    check!(
        runs(&picks, busy) >= 8,
        "the busy task ran {} of 30 ticks against the woken and forked tasks: {picks:?}",
        runs(&picks, busy)
    );
    cleanup();
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "task_sched_idle_wake_keeps_virtual_time",
        idle_wake_keeps_virtual_time,
    ),
    (
        "task_sched_strict_classes_no_starvation",
        strict_classes_no_starvation,
    ),
    (
        "task_sched_weighted_share_within_class",
        weighted_share_within_class,
    ),
    ("task_sched_skips_blocked_and_done", skips_blocked_and_done),
    (
        "task_sched_priority_api_and_cpu_accounting",
        priority_api_and_cpu_accounting,
    ),
];
