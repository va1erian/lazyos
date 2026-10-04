//! Per-task wakeup and context-switch counters (docs/performance-plan.md P7).
//!
//! An idle desktop should leave the CPU halted: every timer a service parks
//! on, and every poll loop, shows up here as wakes (blocked -> runnable) and
//! runs (the scheduler switched to the task). [`report`] prints the
//! cumulative counters of every task that has any, so a host script can diff
//! two lines over a window (`tools/perf/idle.py`):
//!
//! ```text
//! PERF:sched:tick=<t> idle=<idle ticks> <name>#<slot>=<wakes>/<runs> ...
//! ```
//!
//! Counters belong to a slot, not a task: a reused slot keeps counting, which
//! the host side notices as a name change.

use core::fmt::Write;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::task::{self, MAX_TASKS};

static WAKES: [AtomicU32; MAX_TASKS] = [const { AtomicU32::new(0) }; MAX_TASKS];
static RUNS: [AtomicU32; MAX_TASKS] = [const { AtomicU32::new(0) }; MAX_TASKS];

/// `slot` moved from blocked to runnable.
pub fn woke(slot: usize) {
    if let Some(count) = WAKES.get(slot) {
        count.fetch_add(1, Ordering::Relaxed);
    }
}

/// The scheduler switched to `slot`.
pub fn ran(slot: usize) {
    if let Some(count) = RUNS.get(slot) {
        count.fetch_add(1, Ordering::Relaxed);
    }
}

/// Print one `PERF:sched` line with every slot that has counted anything.
pub fn report(now: u64) {
    let snapshot = task::introspect::TaskSnapshot::snapshot();
    let mut out = alloc::string::String::with_capacity(1024);
    let _ = write!(out, "PERF:sched:tick={now} idle={}", task::idle_ticks());
    for (slot, row) in snapshot.rows.iter().enumerate().take(MAX_TASKS) {
        let wakes = WAKES[slot].load(Ordering::Relaxed);
        let runs = RUNS[slot].load(Ordering::Relaxed);
        if wakes == 0 && runs == 0 {
            continue;
        }
        let name = if row.live { row.name.as_str() } else { "-" };
        let _ = write!(out, " {name}#{slot}={wakes}/{runs}");
    }
    out.push('\n');
    super::imp::line(format_args!("{out}"));
}
