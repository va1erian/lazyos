//! What the CPU guard says about its demotions (issue #857): the first
//! demotion of a task at once, then a summary per process every 10 s, never a
//! line per demotion; correctness cases and a soak.

use super::guard_suite::{child, cleanup, fresh, picks};
use super::*;
use crate::task::guard::WINDOW_TICKS;
use crate::task::PriorityClass;

/// A hog's first demotion is named once; a repeat adds nothing until a
/// summary is due, and the summary counts exactly what happened since.
fn first_then_summary() -> Result<(), String> {
    fresh()?;
    let hog = child(PriorityClass::Interactive)?;
    let _peer = child(PriorityClass::Normal)?;
    picks(3 * WINDOW_TICKS as usize);
    let (first, summary, omitted) = task::guard::collect_for_test(false);
    check!(first.contains(&hog), "first lines {first:?}, wanted {hog}");
    check!(
        summary.is_empty() && omitted == 0,
        "a summary came out before it was due: {summary:?}"
    );
    // Nothing new, nothing printed, even with a summary due.
    let (first, summary, _) = task::guard::collect_for_test(true);
    check!(
        first.is_empty() && summary.is_empty(),
        "an unchanged guard reported {first:?} {summary:?}"
    );
    // More demotions: not a first line, and no summary until it is due.
    picks(5 * WINDOW_TICKS as usize);
    let (first, summary, _) = task::guard::collect_for_test(false);
    check!(
        first.is_empty() && summary.is_empty(),
        "a repeat demotion printed before the summary: {first:?} {summary:?}"
    );
    let (first, summary, _) = task::guard::collect_for_test(true);
    check!(first.is_empty(), "a repeat became a first line: {first:?}");
    let row = summary.iter().find(|row| row.0 == hog);
    check!(
        row.is_some_and(|row| row.1 >= 1 && row.2 == 1),
        "the summary was {summary:?}"
    );
    cleanup();
    Ok(())
}

/// Threads of one process share one summary row: the demotions add up and
/// the row counts its threads.
fn summary_rows_are_per_process() -> Result<(), String> {
    fresh()?;
    let leader = child(PriorityClass::Interactive)?;
    let second = child(PriorityClass::Interactive)?;
    task::linuxstate::with_extras_of(second, |extras| extras.tgid = leader);
    let _peer = child(PriorityClass::Normal)?;
    picks(4 * WINDOW_TICKS as usize);
    let (first, _, _) = task::guard::collect_for_test(false);
    check!(
        first.contains(&leader) && first.contains(&second),
        "each new task is named once: {first:?}"
    );
    picks(6 * WINDOW_TICKS as usize);
    let (_, summary, _) = task::guard::collect_for_test(true);
    check!(
        summary.iter().all(|row| row.0 != second),
        "a thread got a row of its own: {summary:?}"
    );
    let row = summary.iter().find(|row| row.0 == leader);
    check!(
        row.is_some_and(|row| row.2 == 2),
        "two threads of one process gave rows {summary:?}"
    );
    cleanup();
    Ok(())
}

/// Soak: many processes demoted over many windows. A call prints at most a
/// handful of first lines and a bounded summary, whatever the demotion rate,
/// and everything demoted is eventually accounted for exactly once.
fn soak_report_is_bounded() -> Result<(), String> {
    fresh()?;
    let mut hogs = Vec::new();
    for _ in 0..20 {
        hogs.push(child(PriorityClass::Interactive)?);
    }
    let _peer = child(PriorityClass::Normal)?;
    let mut named = 0usize;
    let mut summarised = 0u64;
    for round in 0..60 {
        picks(2 * WINDOW_TICKS as usize);
        let due = round % 10 == 9;
        let (first, summary, omitted) = task::guard::collect_for_test(due);
        check!(first.len() <= 4, "round {round}: {} first lines", first.len());
        check!(
            summary.len() <= 8,
            "round {round}: {} summary rows",
            summary.len()
        );
        named += first.len();
        summarised += summary.iter().map(|row| u64::from(row.1)).sum::<u64>();
        check!(
            omitted == 0 || summary.len() == 8,
            "round {round}: rows left out ({omitted}) with room in the summary"
        );
    }
    check!(named <= hogs.len(), "{named} tasks named, only {} exist", hogs.len());
    check!(
        summarised <= task::guard::demotions(),
        "the summaries counted {summarised}, the guard did {}",
        task::guard::demotions()
    );
    cleanup();
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("task_guard_report_first_then_summary", first_then_summary),
    (
        "task_guard_report_rows_are_per_process",
        summary_rows_are_per_process,
    ),
    ("task_guard_report_soak_is_bounded", soak_report_is_bounded),
];
