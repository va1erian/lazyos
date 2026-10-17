//! How long each task waited, runnable, for the CPU: the time between a task becoming runnable (woken,
//! or preempted while it still had work) and the scheduler running it.
//!
//! This is the most a second CPU, or a scheduling policy that favoured the task, could give that task
//! back. It is not the latency a user sees: making the WebProcess's main thread `Interactive` took its
//! wait from 33% to 4% of a scroll and left the page's answer to the wheel where it was
//! (docs/lazywebplus-performance-plan.md section 10), so read a wait as a cost to the task, and check
//! that the thing you care about moves before acting on it. `PERF:wake_run` is the same for woken
//! tasks only, over every task at once; this is per task (a thread's slot), and counts preemption,
//! which is most of what a busy page's main thread sees.
//!
//! ```text
//! PERF:runwait:tick=<t> <name>#<slot>=<waits>/<total us>/<longest us> ...
//! ```
//!
//! Cumulative, and only tasks that waited 0.5 ms or more in all (a host script diffs two lines over a
//! window). A slot that ended while it waited keeps its stamp for the next task in it (that task's first
//! run then reads long); the totals ignore nothing, so read the largest ones with that in mind.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::task::MAX_TASKS;

pub struct Ledger {
    /// The TSC when the task began waiting; 0 when it is not.
    ready_at: [AtomicU64; MAX_TASKS],
    cycles: [AtomicU64; MAX_TASKS],
    waits: [AtomicU32; MAX_TASKS],
    longest: [AtomicU64; MAX_TASKS],
}

impl Ledger {
    pub const fn new() -> Ledger {
        Ledger {
            ready_at: [const { AtomicU64::new(0) }; MAX_TASKS],
            cycles: [const { AtomicU64::new(0) }; MAX_TASKS],
            waits: [const { AtomicU32::new(0) }; MAX_TASKS],
            longest: [const { AtomicU64::new(0) }; MAX_TASKS],
        }
    }

    /// `slot` is runnable and not running, from `now` (a clock that never reads 0).
    pub fn ready(&self, slot: usize, now: u64) {
        if let Some(at) = self.ready_at.get(slot) {
            at.store(now.max(1), Ordering::Relaxed);
        }
    }

    /// `slot` is on the CPU at `now`: what it waited is counted.
    pub fn run(&self, slot: usize, now: u64) {
        let Some(at) = self.ready_at.get(slot) else {
            return;
        };
        let since = at.swap(0, Ordering::Relaxed);
        if since == 0 {
            return;
        }
        let waited = now.wrapping_sub(since);
        self.cycles[slot].fetch_add(waited, Ordering::Relaxed);
        self.waits[slot].fetch_add(1, Ordering::Relaxed);
        self.longest[slot].fetch_max(waited, Ordering::Relaxed);
    }

    /// (waits, total cycles, longest cycles) of `slot`.
    pub fn totals(&self, slot: usize) -> (u32, u64, u64) {
        match self.waits.get(slot) {
            Some(waits) => (
                waits.load(Ordering::Relaxed),
                self.cycles[slot].load(Ordering::Relaxed),
                self.longest[slot].load(Ordering::Relaxed),
            ),
            None => (0, 0, 0),
        }
    }
}

#[cfg(lazyos_perf)]
mod live {
    use core::fmt::Write;

    use super::Ledger;
    use crate::perf::hist::cycles_to_ns;
    use crate::task::{self, MAX_TASKS};

    pub static LEDGER: Ledger = Ledger::new();

    /// Print the `PERF:runwait` line.
    pub fn report(now: u64, per_tick: u64) {
        let snapshot = task::introspect::TaskSnapshot::snapshot();
        let mut out = alloc::string::String::with_capacity(512);
        let _ = write!(out, "PERF:runwait:tick={now}");
        for (slot, row) in snapshot.rows.iter().enumerate().take(MAX_TASKS) {
            let (waits, cycles, longest) = LEDGER.totals(slot);
            let total_us = cycles_to_ns(cycles, per_tick) / 1000;
            if waits == 0 || total_us < 500 {
                continue;
            }
            let name = if row.live { row.name.as_str() } else { "-" };
            let longest_us = cycles_to_ns(longest, per_tick) / 1000;
            let _ = write!(out, " {name}#{slot}={waits}/{total_us}/{longest_us}");
        }
        out.push('\n');
        crate::perf::imp::line(format_args!("{out}"));
    }
}

#[cfg(lazyos_perf)]
pub use live::{report, LEDGER};
