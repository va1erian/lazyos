//! Freeze forensics (issue #861): `PROC:EXIT`, program names, `SCHED:STUCK`,
//! `PERF:CPU`, `LOCK:WAIT` and `SERIAL:BACKLOG`, each judged on the lines it
//! prints (the trace sink is diverted to a buffer) and on its bounds; with
//! soaks that exit, scan and sample far more than the tables hold.

use super::*;
use crate::process;
use crate::serial;
use crate::task::signal::{self, SigInfo};
use crate::task::{
    cpustat, linuxstate, lockwait, procexit, relax, stuck, PriorityClass, TaskState,
};
use crate::trace::capture;

static LINES: spin::Mutex<Vec<u8>> = spin::Mutex::new(Vec::new());

fn store(line: &[u8]) {
    x86_64::instructions::interrupts::without_interrupts(|| LINES.lock().extend_from_slice(line));
}

/// Run `body` with trace lines captured; returns them.
fn captured(body: impl FnOnce()) -> String {
    x86_64::instructions::interrupts::without_interrupts(|| LINES.lock().clear());
    crate::trace::reset_for_test();
    capture::set(store);
    body();
    capture::clear();
    let bytes = x86_64::instructions::interrupts::without_interrupts(|| LINES.lock().clone());
    String::from_utf8_lossy(&bytes).into_owned()
}

fn fresh() -> Result<(), String> {
    if !crate::trace::ENABLED {
        return Err(String::from("tracing is compiled out"));
    }
    task::register_kernel();
    task::harness::reset();
    signal::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(true);
    procexit::reset_for_test();
    stuck::reset_for_test();
    cpustat::reset_for_test();
    lockwait::reset_for_test();
    Ok(())
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
    signal::harness::reset();
    procexit::reset_for_test();
}

fn fork() -> Result<usize, String> {
    task::spawn_fork().map_err(|error| format!("spawn: {error}"))
}

/// `execve` names the process after its program, `PR_SET_NAME` renames one
/// thread, and the next `execve` forgets the rename.
fn exec_sets_the_comm() -> Result<(), String> {
    fresh()?;
    let child = fork()?;
    task::harness::switch_current(child);
    linuxstate::set_exe("/system/bin/webprocess");
    let comm = |slot| {
        let (bytes, len) = linuxstate::comm(slot);
        String::from_utf8_lossy(&bytes[..len]).into_owned()
    };
    check!(
        comm(child) == "webprocess",
        "comm after exec: {}",
        comm(child)
    );
    linuxstate::with_extras(|extras| extras.comm = linuxstate::comm_of_path("renamed"));
    check!(comm(child) == "renamed", "comm after PR_SET_NAME");
    linuxstate::set_exe("/a/very/long/program-name-over-fifteen");
    check!(
        comm(child) == "program-name-ov",
        "a long name is cut at 15 bytes: {}",
        comm(child)
    );
    check!(
        linuxstate::comm_of_path("/x/") == linuxstate::comm_of_path(""),
        "a trailing slash names nothing"
    );
    cleanup();
    Ok(())
}

/// One process ending prints one line with its name, status, parent and CPU.
fn exit_line_names_the_process() -> Result<(), String> {
    fresh()?;
    let child = fork()?;
    task::harness::switch_current(child);
    linuxstate::set_exe("/system/bin/worker");
    task::harness::switch_current(task::KERNEL_TASK);
    let text = captured(|| {
        task::harness::finish(child, 3);
        procexit::report();
    });
    let want = format!("PROC:EXIT tgid={child} status=3 sig=- by=- parent=0");
    check!(text.contains(&want), "no `{want}` in {text:?}");
    check!(text.contains("comm=worker"), "no program name in {text:?}");
    check!(text.matches("PROC:EXIT").count() == 1, "{text:?}");
    // Reported once.
    let again = captured(procexit::report);
    check!(again.is_empty(), "the exit was printed twice: {again:?}");
    cleanup();
    Ok(())
}

/// A process with several threads prints when its last thread dies, with the
/// process's id (the leader's slot) whichever thread was last.
fn exit_line_waits_for_the_last_thread() -> Result<(), String> {
    fresh()?;
    let leader = fork()?;
    task::harness::switch_current(leader);
    let thread = task::spawn_thread("worker-thread", process::USER_STACK_TOP, 0, 0)
        .map_err(|error| format!("thread: {error}"))?;
    task::harness::switch_current(task::KERNEL_TASK);
    let text = captured(|| {
        task::harness::finish(leader, 0);
        procexit::report();
    });
    check!(
        text.is_empty(),
        "the process printed with a thread alive: {text:?}"
    );
    let text = captured(|| {
        task::harness::finish(thread, 0);
        procexit::report();
    });
    check!(
        text.contains(&format!("PROC:EXIT tgid={leader} ")),
        "no exit named {leader} in {text:?}"
    );
    check!(text.matches("PROC:EXIT").count() == 1, "{text:?}");
    cleanup();
    Ok(())
}

/// A process killed by a signal says which signal and who sent it.
fn exit_line_names_the_killer() -> Result<(), String> {
    fresh()?;
    let killer = fork()?;
    let victim = fork()?;
    task::harness::switch_current(killer);
    signal::send_to_slot(
        killer,
        victim,
        signal::SIGKILL,
        SigInfo::user(killer, signal::SI_USER),
    )
    .map_err(|error| format!("SIGKILL: {error:?}"))?;
    check!(
        task::harness::set_user_frame(victim, 0x40_0500, 0x70_0800),
        "could not shape the victim's frame"
    );
    task::harness::switch_current(task::KERNEL_TASK);
    let text = captured(|| {
        task::harness::run_sweep();
        task::reclaim_pending();
        procexit::report();
    });
    let want = format!("PROC:EXIT tgid={victim} status=0 sig=9 by={killer} ");
    check!(text.contains(&want), "no `{want}` in {text:?}");
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

/// A runnable task inside the kernel is reported after two seconds, once,
/// then every ten; a blocked one or one in user mode never is.
fn stuck_reports_a_runnable_kernel_task() -> Result<(), String> {
    fresh()?;
    let spinner = fork()?;
    let sleeper = fork()?;
    let user = fork()?;
    task::harness::set_kernel_frame(spinner);
    task::harness::set_kernel_frame(sleeper);
    task::harness::set_state(
        sleeper,
        TaskState::Blocked {
            wait: task::WaitKind::Sleep,
            deadline: None,
        },
    );
    stuck::sample_at_for_test(1000);
    check!(
        stuck::watched_for_test(spinner).is_some(),
        "a runnable kernel task was not watched"
    );
    check!(
        stuck::watched_for_test(sleeper).is_none() && stuck::watched_for_test(user).is_none(),
        "a blocked or user-mode task was watched"
    );
    let early = captured(|| stuck::sample_at_for_test(1199));
    check!(early.is_empty(), "reported before two seconds: {early:?}");
    let text = captured(|| stuck::sample_at_for_test(1200));
    check!(
        text.contains(&format!("SCHED:STUCK tgid={spinner} thread={spinner}/")),
        "no report for {spinner} in {text:?}"
    );
    check!(text.contains("krip=0x"), "no kernel rip: {text:?}");
    check!(text.matches("SCHED:STUCK").count() == 1, "{text:?}");
    let quiet = captured(|| stuck::sample_at_for_test(1900));
    check!(quiet.is_empty(), "repeated within ten seconds: {quiet:?}");
    let again = captured(|| stuck::sample_at_for_test(2200));
    check!(again.contains("SCHED:STUCK"), "no repeat after ten seconds");
    // It returns to user mode: forgotten.
    let _ = fork_user_frame(spinner);
    stuck::sample_at_for_test(2300);
    check!(
        stuck::watched_for_test(spinner).is_none(),
        "a task back in user mode is still watched"
    );
    cleanup();
    Ok(())
}

fn fork_user_frame(slot: usize) -> bool {
    task::harness::set_user_frame(slot, 0x40_0000, 0x70_0000)
        && task::harness::set_frame_cs(slot, 0x23)
}

/// Soak: many tasks, each entering and leaving the kernel over many samples;
/// a sample prints at most its row limit and the watch table never leaks.
fn soak_stuck_sampling_is_bounded() -> Result<(), String> {
    fresh()?;
    let mut slots = Vec::new();
    for _ in 0..30 {
        let slot = fork()?;
        task::harness::set_kernel_frame(slot);
        slots.push(slot);
    }
    let mut now = 1000;
    let mut worst = 0;
    for round in 0..120 {
        now += 50;
        let text = captured(|| stuck::sample_at_for_test(now));
        let lines = text.matches("SCHED:STUCK").count();
        worst = worst.max(lines);
        check!(lines <= 8, "round {round}: {lines} lines in one sample");
        if round % 40 == 39 {
            for &slot in &slots {
                fork_user_frame(slot);
            }
            stuck::sample_at_for_test(now + 1);
            for &slot in &slots {
                check!(
                    stuck::watched_for_test(slot).is_none(),
                    "slot {slot} still watched after returning to user mode"
                );
                task::harness::set_kernel_frame(slot);
            }
        }
    }
    check!(worst > 0, "thirty stuck tasks never reported");
    cleanup();
    Ok(())
}

/// `PERF:CPU` stays quiet on an idle machine and names the busy processes,
/// busiest first, with a share that adds up.
fn cpu_report_names_the_busy() -> Result<(), String> {
    fresh()?;
    let a = fork()?;
    let b = fork()?;
    task::harness::switch_current(a);
    linuxstate::set_exe("/system/bin/spinner");
    task::harness::switch_current(task::KERNEL_TASK);
    for slot in [a, b] {
        task::set_priority(slot, PriorityClass::Normal);
    }
    check!(
        captured(|| cpustat::sample_at_for_test(500)).is_empty(),
        "the baseline look printed"
    );
    for _ in 0..500 {
        task::harness::simulate_tick();
    }
    let text = captured(|| cpustat::sample_at_for_test(1000));
    check!(text.contains("PERF:CPU tgid="), "no CPU line in {text:?}");
    check!(text.contains("comm=spinner"), "no program name in {text:?}");
    let total: u64 = text
        .lines()
        .filter_map(|line| line.split("pct=").nth(1))
        .filter_map(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
        .sum();
    check!(
        (90..=100).contains(&total),
        "shares add up to {total}% in {text:?}"
    );
    for slot in [a, b] {
        task::harness::set_state(
            slot,
            TaskState::Blocked {
                wait: task::WaitKind::Sleep,
                deadline: None,
            },
        );
    }
    for _ in 0..500 {
        task::harness::simulate_tick();
    }
    let quiet = captured(|| cpustat::sample_at_for_test(1500));
    check!(quiet.is_empty(), "an idle period printed: {quiet:?}");
    cleanup();
    Ok(())
}

/// Soak: more processes than the table follows, sampled repeatedly; no
/// panic, no more than five lines a look.
fn soak_cpu_with_many_processes() -> Result<(), String> {
    fresh()?;
    for _ in 0..(cpustat::CAPACITY + 8).min(200) {
        fork()?;
    }
    cpustat::sample_at_for_test(500);
    for look in 1..8u64 {
        for _ in 0..500 {
            task::harness::simulate_tick();
        }
        let text = captured(|| cpustat::sample_at_for_test(500 + look * 500));
        check!(
            text.matches("PERF:CPU").count() <= 5,
            "look {look}: {} lines",
            text.matches("PERF:CPU").count()
        );
    }
    cleanup();
    Ok(())
}

/// A contender that waited half a second is reported, once per five
/// seconds; a short wait or a stale one is not.
fn lock_wait_reports_a_long_wait() -> Result<(), String> {
    fresh()?;
    let slow = fork()?;
    let brief = fork()?;
    const S: u64 = 1_000_000_000;
    let now = 100 * S;
    relax::set_waiting_for_test(slow, now - 600_000_000, now - 1_000_000);
    relax::set_waiting_for_test(brief, now - 100_000_000, now - 1_000_000);
    let reported = lockwait::sample_at_for_test(now);
    check!(reported == [slow], "reported {reported:?}, wanted [{slow}]");
    // Waiting on: the same wait, a second later, is not said again.
    relax::set_waiting_for_test(slow, now - 600_000_000, now + S - 1_000_000);
    check!(
        lockwait::sample_at_for_test(now + S).is_empty(),
        "the same wait was reported within five seconds"
    );
    relax::set_waiting_for_test(slow, now - 600_000_000, now + 6 * S - 1_000_000);
    check!(
        lockwait::sample_at_for_test(now + 6 * S) == [slow],
        "a wait lasting six seconds was not reported again"
    );
    // The wait ended long ago.
    check!(
        lockwait::sample_at_for_test(now + 60 * S).is_empty(),
        "a finished wait was reported"
    );
    let text = captured(|| {
        relax::set_waiting_for_test(slow, now, now + 70 * S - 1_000_000);
        lockwait::sample_at_for_test(now + 70 * S);
    });
    check!(text.contains("LOCK:WAIT thread="), "no line: {text:?}");
    check!(text.contains("waiters=1"), "no waiter count: {text:?}");
    for slot in [slow, brief] {
        relax::set_waiting_for_test(slot, 0, 0);
    }
    cleanup();
    Ok(())
}

/// The serial backlog is reported only above 4 KiB, at most every 5 s, with
/// the longest port access.
fn serial_backlog_is_reported() -> Result<(), String> {
    let port = serial::divert_for_test(0);
    let mut seen = String::new();
    let line = "SERIALTEST:backlog.......................................................\n";
    let quiet = captured(|| serial::report_backlog_for_test(10_000));
    for _ in 0..50 {
        serial::_write_str(line);
    }
    let low = serial::queued_len();
    let early = captured(|| serial::report_backlog_for_test(20_000));
    seen.push_str(&early);
    while serial::queued_len() <= 5000 {
        serial::_write_str(line);
    }
    let text = captured(|| serial::report_backlog_for_test(30_000));
    drop(port);
    check!(quiet.is_empty(), "an empty ring reported: {quiet:?}");
    check!(
        low < 4096 && seen.is_empty(),
        "a small backlog reported: {seen:?}"
    );
    check!(
        text.contains("SERIAL:BACKLOG bytes=") && text.contains("max_io_us="),
        "no backlog line: {text:?}"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("forensics_exec_sets_the_comm", exec_sets_the_comm),
    (
        "forensics_exit_line_names_the_process",
        exit_line_names_the_process,
    ),
    (
        "forensics_exit_line_waits_for_the_last_thread",
        exit_line_waits_for_the_last_thread,
    ),
    (
        "forensics_exit_line_names_the_killer",
        exit_line_names_the_killer,
    ),
    (
        "forensics_soak_exits_overflow_the_table",
        soak_exits_overflow_the_table,
    ),
    (
        "forensics_stuck_reports_a_runnable_kernel_task",
        stuck_reports_a_runnable_kernel_task,
    ),
    (
        "forensics_soak_stuck_sampling_is_bounded",
        soak_stuck_sampling_is_bounded,
    ),
    (
        "forensics_cpu_report_names_the_busy",
        cpu_report_names_the_busy,
    ),
    (
        "forensics_soak_cpu_with_many_processes",
        soak_cpu_with_many_processes,
    ),
    (
        "forensics_lock_wait_reports_a_long_wait",
        lock_wait_reports_a_long_wait,
    ),
    (
        "forensics_serial_backlog_is_reported",
        serial_backlog_is_reported,
    ),
];
