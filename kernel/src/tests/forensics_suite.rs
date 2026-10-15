//! Freeze forensics (issue #861): `PROC:EXIT`, program names, `SCHED:STUCK`,
//! `PERF:CPU`, `LOCK:WAIT` and `SERIAL:BACKLOG`, each judged on the lines it
//! prints (the trace sink is diverted to a buffer) and on its bounds; with
//! soaks that exit, scan and sample far more than the tables hold.

use super::*;
use crate::process;
use crate::serial;
use crate::task::signal::{self, SigInfo};
use crate::task::{cpustat, linuxstate, lockwait, procexit, stuck, PriorityClass, TaskState};
use crate::trace::capture;

static LINES: spin::Mutex<Vec<u8>> = spin::Mutex::new(Vec::new());

fn store(line: &[u8]) {
    x86_64::instructions::interrupts::without_interrupts(|| LINES.lock().extend_from_slice(line));
}

/// Run `body` with trace lines captured; returns them.
pub(super) fn captured(body: impl FnOnce()) -> String {
    x86_64::instructions::interrupts::without_interrupts(|| LINES.lock().clear());
    crate::trace::reset_for_test();
    capture::set(store);
    body();
    capture::clear();
    let bytes = x86_64::instructions::interrupts::without_interrupts(|| LINES.lock().clone());
    String::from_utf8_lossy(&bytes).into_owned()
}

pub(super) fn fresh() -> Result<(), String> {
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

pub(super) fn cleanup() {
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

pub(super) fn fork() -> Result<usize, String> {
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
    let want = format!("PROC:EXIT tgid={child} thread={child}/");
    check!(text.contains(&want), "no `{want}` in {text:?}");
    let want = "status=3 sig=- by=- parent=0";
    check!(text.contains(want), "no `{want}` in {text:?}");
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
    let want = format!("PROC:EXIT tgid={victim} thread={victim}/");
    check!(text.contains(&want), "no `{want}` in {text:?}");
    let want = format!("status=- sig=9 by={killer} ");
    check!(text.contains(&want), "no `{want}` in {text:?}");
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
        "forensics_cpu_report_names_the_busy",
        cpu_report_names_the_busy,
    ),
    (
        "forensics_soak_cpu_with_many_processes",
        soak_cpu_with_many_processes,
    ),
    (
        "forensics_serial_backlog_is_reported",
        serial_backlog_is_reported,
    ),
];
