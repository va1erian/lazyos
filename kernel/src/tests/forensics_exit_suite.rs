//! Freeze forensics (issue #861): what `PROC:EXIT` says and the CPU books it
//! shares with `PERF:CPU`: how a process died and who sent the signal, which
//! tasks form a process, what a flood of exits may print, and the CPU of
//! threads that were reaped. Soak at the end.

use super::forensics_suite::{captured, cleanup, fork, fresh};
use super::*;
use crate::process;
use crate::task::signal::{self, SigInfo};
use crate::task::{cpustat, linuxstate, procexit, PriorityClass};

/// How a process died is what the kernel recorded for `wait4`, not a guess
/// from the status: `exit(137)` is a status, a recorded `SIGKILL` a signal.
fn exit_137_is_a_status_not_a_signal() -> Result<(), String> {
    fresh()?;
    let exited = fork()?;
    let killed = fork()?;
    linuxstate::with_extras_of(killed, |extras| extras.term_signal = 9);
    let text = captured(|| {
        task::harness::finish(exited, 137);
        task::harness::finish(killed, 137);
        procexit::report();
    });
    let line = |tgid: usize| {
        text.lines()
            .find(|l| l.contains(&format!("PROC:EXIT tgid={tgid} ")))
            .map(String::from)
            .unwrap_or_default()
    };
    check!(
        line(exited).contains("status=137 sig=-"),
        "exit(137) printed as {}",
        line(exited)
    );
    check!(
        line(killed).contains("status=- sig=9"),
        "a recorded SIGKILL printed as {}",
        line(killed)
    );
    cleanup();
    Ok(())
}

/// A sender is remembered only for a signal that will end the process, and
/// forgotten when the process ends however it ends: a signal that was
/// queued but never decided its death is not blamed, and a recycled page
/// table does not inherit it.
fn stale_sender_is_not_blamed() -> Result<(), String> {
    fresh()?;
    let sender = fork()?;
    let victim = fork()?;
    task::harness::switch_current(sender);
    signal::send_to_slot(
        sender,
        victim,
        signal::SIGTERM,
        SigInfo::user(sender, signal::SI_USER),
    )
    .map_err(|error| format!("SIGTERM: {error:?}"))?;
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        procexit::senders_for_test() == 1,
        "the default-fatal SIGTERM was not remembered"
    );
    // The victim exits by itself before the signal is taken.
    let text = captured(|| {
        task::harness::finish(victim, 0);
        procexit::report();
    });
    check!(
        text.contains("status=0 sig=- by=-"),
        "a clean exit was blamed on a sender: {text:?}"
    );
    check!(
        procexit::senders_for_test() == 0,
        "{} senders outlived their process",
        procexit::senders_for_test()
    );
    cleanup();
    Ok(())
}

/// A `vfork` child (a copy-on-write clone here) is a process of its own: its
/// `_exit` is reported while its parent runs on.
fn vfork_child_exit_is_reported() -> Result<(), String> {
    fresh()?;
    let parent = fork()?;
    task::harness::switch_current(parent);
    let child =
        task::spawn_vfork(process::USER_STACK_TOP).map_err(|error| format!("vfork: {error}"))?;
    task::harness::switch_current(task::KERNEL_TASK);
    let text = captured(|| {
        task::harness::finish(child, 0);
        procexit::report();
    });
    check!(
        text.contains(&format!("PROC:EXIT tgid={child} "))
            && text.contains(&format!("parent={parent}")),
        "no exit of the vfork child {child} under {parent} in {text:?}"
    );
    cleanup();
    Ok(())
}

/// A shell loop's hundreds of clean exits cannot crowd out the one exit that
/// matters: clean exits have a budget of their own.
fn clean_exits_do_not_crowd_out_a_crash() -> Result<(), String> {
    fresh()?;
    let mut text = String::new();
    for _ in 0..4 {
        let mut batch = Vec::new();
        for _ in 0..30 {
            batch.push(fork()?);
        }
        for &child in &batch {
            task::harness::finish(child, 0);
        }
        while task::reap_child().is_some() {}
        text.push_str(&captured(procexit::report));
    }
    let crashed = fork()?;
    linuxstate::with_extras_of(crashed, |extras| extras.term_signal = 11);
    text.push_str(&captured(|| {
        task::harness::finish(crashed, 139);
        procexit::report();
    }));
    let clean = text.matches("status=0 sig=-").count();
    check!(
        clean <= 25,
        "{clean} clean exits printed in one window: not budgeted"
    );
    check!(
        text.contains(&format!("PROC:EXIT tgid={crashed} ")) && text.contains("sig=11"),
        "the crash was crowded out: {} lines",
        text.lines().count()
    );
    cleanup();
    Ok(())
}

/// Ticks of threads that were reaped stay with their process: the process
/// does not look idle once a worker thread is gone, and `PROC:EXIT` counts
/// the CPU of the whole life.
fn reaped_threads_keep_their_cpu() -> Result<(), String> {
    fresh()?;
    let leader = fork()?;
    task::harness::switch_current(leader);
    let thread = task::spawn_thread("worker", process::USER_STACK_TOP, 0, 0)
        .map_err(|error| format!("thread: {error}"))?;
    task::harness::switch_current(task::KERNEL_TASK);
    for slot in [leader, thread] {
        task::set_priority(slot, PriorityClass::Normal);
    }
    cpustat::sample_at_for_test(500);
    for _ in 0..250 {
        task::harness::simulate_tick();
    }
    check!(task::cpu_ticks(thread) > 0, "the thread never ran");
    // The worker ends mid-period. (A thread has no parent to reap it: the next
    // tick flags it and the task-context reclaim frees the slot.)
    task::harness::finish(thread, 0);
    task::harness::simulate_tick();
    let thread_ticks = task::cpu_ticks(thread);
    task::reclaim_pending();
    check!(
        task::harness::state(thread).is_none(),
        "the finished thread was not reclaimed"
    );
    check!(
        cpustat::retired_for_test(leader) == thread_ticks,
        "retired {} of {thread_ticks} ticks",
        cpustat::retired_for_test(leader)
    );
    for _ in 0..250 {
        task::harness::simulate_tick();
    }
    let text = captured(|| cpustat::sample_at_for_test(1000));
    let pct: u64 = text
        .lines()
        .filter_map(|line| line.split("pct=").nth(1))
        .filter_map(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
        .sum();
    check!(
        (95..=100).contains(&pct),
        "the process shows {pct}% after a thread was reaped: {text:?}"
    );
    // The process ends: its exit line has the CPU of both threads.
    let leader_ticks = task::cpu_ticks(leader);
    let text = captured(|| {
        task::harness::finish(leader, 0);
        procexit::report();
    });
    let want = format!("cpu_ms={}", (leader_ticks + thread_ticks) * 10);
    check!(text.contains(&want), "no `{want}` in {text:?}");
    check!(
        cpustat::retired_for_test(leader) == 0,
        "the accumulator outlived the process"
    );
    cleanup();
    Ok(())
}

/// Soak: far more exits than the table holds between two reports. Nothing
/// panics, what is printed is bounded, and the loss is counted and said.
fn soak_exits_overflow_the_table() -> Result<(), String> {
    fresh()?;
    let mut printed = 0;
    let mut lost_said = false;
    for round in 0..6 {
        let mut children = Vec::new();
        for _ in 0..80 {
            children.push(fork()?);
        }
        for &child in &children {
            task::harness::finish(child, 1);
        }
        while task::reap_child().is_some() {}
        let (pending, lost) = procexit::pending_for_test();
        check!(pending <= 64, "round {round}: {pending} exits pending");
        procexit::reset_budgets_for_test();
        let text = captured(procexit::report);
        printed += text.matches("PROC:EXIT").count();
        lost_said |= text.contains("PROC:LOST");
        check!(
            text.matches("PROC:EXIT").count() as u64 + lost >= 80,
            "round {round}: 80 exits, {} printed, {lost} lost",
            text.matches("PROC:EXIT").count()
        );
        check!(
            procexit::pending_for_test().0 == 0,
            "round {round}: exits left after a report"
        );
    }
    check!(printed > 0, "no exit was printed");
    check!(lost_said, "exits were lost and nothing said so");
    cleanup();
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "forensics_exit_137_is_a_status_not_a_signal",
        exit_137_is_a_status_not_a_signal,
    ),
    (
        "forensics_stale_sender_is_not_blamed",
        stale_sender_is_not_blamed,
    ),
    (
        "forensics_vfork_child_exit_is_reported",
        vfork_child_exit_is_reported,
    ),
    (
        "forensics_clean_exits_do_not_crowd_out_a_crash",
        clean_exits_do_not_crowd_out_a_crash,
    ),
    (
        "forensics_reaped_threads_keep_their_cpu",
        reaped_threads_keep_their_cpu,
    ),
    (
        "forensics_soak_exits_overflow_the_table",
        soak_exits_overflow_the_table,
    ),
];
