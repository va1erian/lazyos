//! What the CPU guard says about its demotions (issue #857): the first
//! demotion of a process at once, then a summary per process every 10 s.

use super::*;
use alloc::string::String;

/// Ticks (100 Hz) between two summaries.
const SUMMARY_TICKS: u64 = 1000;
/// Most first-demotion lines one call prints; the rest wait for the next.
const FIRST_LINES_MAX: usize = 4;
/// Most processes a summary names.
const SUMMARY_ROWS_MAX: usize = 8;

/// One task or process in a report.
struct Row {
    name: &'static str,
    slot: usize,
    tgid: usize,
    comm: String,
    /// The task's total demotions (a first line), or the demotions of all the
    /// process's threads since the last summary.
    count: u32,
    threads: u32,
}

/// What the guard has to say about the demotions since the last call.
#[derive(Default)]
struct Findings {
    /// Tasks demoted for the first time: reported at once.
    first: Vec<Row>,
    /// Processes demoted again since the last summary (only when one is due).
    summary: Vec<Row>,
    /// Processes the summary left out for lack of room.
    omitted: usize,
}

/// Print what the guard did, in few lines (issue #857): the **first**
/// demotion of a task at once (`SCHED:GUARD:DEMOTED task=.. comm=.. slot=..`),
/// then one `SCHED:GUARD:SUMMARY` line per process every 10 s with the
/// demotions of all its threads since. A task the guard keeps demoting every
/// window used to print a line a second, and every line refilled the serial
/// ring it was demoted for waiting on.
///
/// Task context only (it takes the serial lock); the multiplexer loop calls it
/// with interrupts off, as a kernel section so the serial drain can open
/// interrupt windows.
pub fn report() {
    static LAST_SUMMARY: AtomicU64 = AtomicU64::new(0);
    if demotions() == 0 {
        return;
    }
    let now = ticks();
    let since = now.wrapping_sub(LAST_SUMMARY.load(Ordering::Relaxed));
    let due = since >= SUMMARY_TICKS;
    let findings = collect(due);
    if due && !findings.summary.is_empty() {
        LAST_SUMMARY.store(now, Ordering::Relaxed);
    }
    print(&findings, since / 100);
}

/// Take the unreported demotions out of the task table. A task whose first
/// demotion this is gets its own line; later ones add to the process's
/// summary row once `summary_due`.
fn collect(summary_due: bool) -> Findings {
    let mut found = Findings::default();
    let mut tasks = TASKS.lock();
    // The processes already named (or waiting for their summary), found in
    // one pass over the table: asking per candidate would walk it again and
    // again with interrupts off and the table locked.
    let mut named = [false; MAX_TASKS];
    for (index, other) in tasks.iter().enumerate() {
        if let Some(other) = other {
            if other.guard.reported > 0 || other.guard.quiet {
                named[match other.linux.tgid {
                    0 => index,
                    leader => leader,
                }] = true;
            }
        }
    }
    for slot in 0..MAX_TASKS {
        let Some(task) = tasks[slot].as_ref() else {
            continue;
        };
        if task.guard.demotions == task.guard.reported {
            continue;
        }
        let tgid = match task.linux.tgid {
            0 => slot,
            leader => leader,
        };
        if task.guard.reported == 0 && !task.guard.quiet {
            // Only the first demotion of a *process* is a line of its own:
            // later threads wait for the process's summary.
            if named[tgid] {
                if let Some(task) = tasks[slot].as_mut() {
                    task.guard.quiet = true;
                }
                continue;
            }
            if found.first.len() >= FIRST_LINES_MAX {
                continue;
            }
            found
                .first
                .push(row(task, slot, tgid, task.guard.demotions));
            named[tgid] = true;
        } else if summary_due {
            let since = task.guard.demotions - task.guard.reported;
            if let Some(existing) = found.summary.iter_mut().find(|row| row.tgid == tgid) {
                existing.count += since;
                existing.threads += 1;
            } else if found.summary.len() < SUMMARY_ROWS_MAX {
                found.summary.push(row(task, slot, tgid, since));
            } else {
                found.omitted += 1;
                continue;
            }
        } else {
            continue;
        }
        if let Some(task) = tasks[slot].as_mut() {
            task.guard.reported = task.guard.demotions;
        }
    }
    found
}

fn row(task: &Task, slot: usize, tgid: usize, count: u32) -> Row {
    let (bytes, len) = linuxstate::comm_of(task);
    Row {
        name: task.name,
        slot,
        tgid,
        comm: String::from_utf8_lossy(&bytes[..len]).into_owned(),
        count,
        threads: 1,
    }
}

/// Print `found` (the table lock is released).
fn print(found: &Findings, window_s: u64) {
    for row in &found.first {
        emit(format_args!(
            "DEMOTED task={} comm={} slot={} tgid={} total={}",
            row.name, row.comm, row.slot, row.tgid, row.count
        ));
    }
    for row in &found.summary {
        emit(format_args!(
            "SUMMARY tgid={} comm={} demotions={} threads={} window_s={}",
            row.tgid, row.comm, row.count, row.threads, window_s
        ));
    }
    if found.omitted > 0 {
        emit(format_args!("SUMMARY more={}", found.omitted));
    }
}

/// One `SCHED:GUARD:` line. A serial line, not a trace: a task being demoted
/// is something an operator wants on a PC with no `dbgd` (docs/tracing-plan.md).
fn emit(detail: core::fmt::Arguments) {
    crate::serial_println!("SCHED:GUARD:{detail}");
}

/// What [`collect`] found, as `(first lines, [(tgid, count, threads)], omitted)`
/// (the kernel suite).
#[cfg(lazyos_tests)]
pub fn collect_for_test(summary_due: bool) -> (Vec<usize>, Vec<(usize, u32, u32)>, usize) {
    let found = collect(summary_due);
    (
        found.first.iter().map(|row| row.slot).collect(),
        found
            .summary
            .iter()
            .map(|row| (row.tgid, row.count, row.threads))
            .collect(),
        found.omitted,
    )
}
