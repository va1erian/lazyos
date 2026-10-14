//! The CPU guard: strict classes with a bound on how long any task can sit in
//! one.
//!
//! Classes are strict: a runnable task beats every task in a lower class, and
//! inside a class the stride scheduler shares by weight. That leaves two ways
//! for the desktop to freeze for seconds under load:
//!
//! * an `Interactive` task that stays busy (a compositor repainting a big
//!   window on a slow CPU) starves every `Normal` service and app it waits on;
//! * a `Normal` task that takes most of the CPU (a course generator, a
//!   package install) starves its peers whenever the fair share fails to hold
//!   them to their turn, and nothing below it can ever run first.
//!
//! The guard bounds both. Time is cut into windows of [`WINDOW_TICKS`] ticks
//! (100 ms):
//!
//! * once the `Interactive` class has used [`BUDGET_TICKS`] of a window, each
//!   `Interactive` task that runs is demoted to `Normal` for the rest of it
//!   (the budget is the class's, so several small tasks are bounded too);
//! * a `Normal` task that has run [`NORMAL_BUDGET_TICKS`] of a window is
//!   demoted to `Background` for the rest of it: every other `Normal` task
//!   now outranks it, while it still gets the CPU whenever nothing else wants
//!   it, so an otherwise idle machine loses nothing.
//!
//! A demoted task gets its class back at the window's end, so a priority
//! inversion (a demoted task holding what a higher one waits for) lasts at
//! most one window. Tasks that sleep through most of each window (input
//! drivers, a compositor at rest, a service answering requests) never reach a
//! budget and are never touched. `Realtime` is left to its callers.

use super::*;
use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Ticks (100 Hz) in one accounting window.
pub const WINDOW_TICKS: u64 = 10;
/// Ticks of a window the `Interactive` class may use, all its tasks together,
/// before the task that is running is demoted.
pub const BUDGET_TICKS: u64 = 5;
/// Ticks of a window one `Normal` task may use before it is demoted.
pub const NORMAL_BUDGET_TICKS: u8 = 6;

/// Per-task guard state, kept in [`Task`].
#[derive(Clone, Copy, Debug)]
pub struct State {
    /// Ticks run this window while `Normal`.
    used: u8,
    /// The class to give back at the window's end while demoted.
    home: Option<PriorityClass>,
    /// The weight to give back with it (a configured one, `set_weight`).
    weight: u16,
    /// Demotions since the task started.
    demotions: u32,
    /// Demotions already reported ([`report`]).
    reported: u32,
    /// Timer ticks that found the task in ring 3 (`PERF:CPU`'s user share).
    user_ticks: u64,
}

impl State {
    pub const fn new() -> State {
        State {
            used: 0,
            home: None,
            weight: 0,
            demotions: 0,
            reported: 0,
            user_ticks: 0,
        }
    }

    /// A timer tick found the task in user mode.
    pub fn note_user_tick(&mut self) {
        self.user_ticks = self.user_ticks.saturating_add(1);
    }

    /// Ticks spent in user mode since the task started.
    pub fn user_ticks(&self) -> u64 {
        self.user_ticks
    }

    /// Demotions since the task started.
    pub fn demotions(&self) -> u32 {
        self.demotions
    }

    /// The class the task holds when the guard is not acting: its own while
    /// not demoted, the one to be restored while demoted.
    pub fn home(&self, current: PriorityClass) -> PriorityClass {
        self.home.unwrap_or(current)
    }

    /// A class was assigned explicitly: forget any pending restore.
    pub(super) fn assigned(&mut self) {
        self.home = None;
        self.used = 0;
    }

    /// Assign `weight` to the task whose live weight is `live`: while demoted
    /// the live weight is the demotion's, and the new one is what the window's
    /// end restores.
    pub(super) fn assign_weight(&mut self, live: &mut u16, weight: u16) {
        if self.home.is_some() {
            self.weight = weight;
        } else {
            *live = weight;
        }
    }

    /// The class and weight a child (`clone`, `fork`) starts with: the
    /// parent's assigned ones, never a demotion that is about to be undone.
    pub fn inherited(&self, class: PriorityClass, weight: u16) -> (PriorityClass, u16) {
        match self.home {
            Some(home) => (home, self.weight),
            None => (class, weight),
        }
    }

    /// Record a demotion from `from` with `weight`; the first one in a window
    /// fixes the class and weight to restore.
    fn demote(&mut self, from: PriorityClass, weight: u16) {
        if self.home.is_none() {
            self.home = Some(from);
            self.weight = weight;
        }
        self.demotions = self.demotions.saturating_add(1);
    }
}

/// Ticks into the current window.
static POSITION: AtomicU64 = AtomicU64::new(0);
/// Ticks the `Interactive` class has used this window.
static CLASS_USED: AtomicU64 = AtomicU64::new(0);
/// Demotions since boot.
static DEMOTIONS: AtomicU64 = AtomicU64::new(0);
/// Whether the guard acts. Always on, except that the kernel test suites
/// that check the stride scheduler's own shares switch it off.
static ENABLED: AtomicBool = AtomicBool::new(cfg!(not(lazyos_tests)));

/// Demotions since boot (the `SCHED:GUARD` report and the tests).
pub fn demotions() -> u64 {
    DEMOTIONS.load(Ordering::Relaxed)
}

/// Switch the guard on or off (test builds only: the production guard is
/// always on).
#[cfg(lazyos_tests)]
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Start a fresh window, so a test counts whole windows from tick one.
#[cfg(lazyos_tests)]
pub fn reset_window() {
    POSITION.store(0, Ordering::Relaxed);
    CLASS_USED.store(0, Ordering::Relaxed);
}

/// One timer tick was charged to `cur` (`ran`: it was runnable, not parked).
/// Call with the task table locked, after the charge
/// (`schedule::charge_tick`), so a simulated tick and a real one run the same
/// rules.
pub(super) fn on_tick(tasks: &mut [Option<Task>; MAX_TASKS], cur: usize, ran: bool) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if ran {
        charge(tasks, cur);
    }
    let position = POSITION.fetch_add(1, Ordering::Relaxed) + 1;
    if position >= WINDOW_TICKS {
        POSITION.store(0, Ordering::Relaxed);
        CLASS_USED.store(0, Ordering::Relaxed);
        new_window(tasks);
    }
}

/// Book one run tick to `cur` and demote it if it is past a budget.
fn charge(tasks: &mut [Option<Task>; MAX_TASKS], cur: usize) {
    let Some(task) = tasks[cur].as_mut() else {
        return;
    };
    let class = task.class;
    let target = match class {
        PriorityClass::Interactive => {
            let used = CLASS_USED.fetch_add(1, Ordering::Relaxed) + 1;
            (used > BUDGET_TICKS).then_some(PriorityClass::Normal)
        }
        PriorityClass::Normal => {
            task.guard.used = task.guard.used.saturating_add(1);
            (task.guard.used > NORMAL_BUDGET_TICKS).then_some(PriorityClass::Background)
        }
        _ => None,
    };
    if let Some(target) = target {
        task.guard.demote(class, task.weight);
        task.class = target;
        task.weight = target.default_weight();
        DEMOTIONS.fetch_add(1, Ordering::Relaxed);
        runq::sync(tasks, cur);
    }
}

/// A window ended: clear every budget and give demoted tasks their class back.
fn new_window(tasks: &mut [Option<Task>; MAX_TASKS]) {
    for slot in 0..MAX_TASKS {
        let Some(task) = tasks[slot].as_mut() else {
            continue;
        };
        task.guard.used = 0;
        if let Some(home) = task.guard.home.take() {
            task.class = home;
            task.weight = task.guard.weight;
            runq::sync(tasks, slot);
        }
    }
}

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
    for slot in 0..MAX_TASKS {
        let Some(task) = tasks[slot].as_mut() else {
            continue;
        };
        if task.guard.demotions == task.guard.reported {
            continue;
        }
        let tgid = match task.linux.tgid {
            0 => slot,
            leader => leader,
        };
        if task.guard.reported == 0 {
            if found.first.len() >= FIRST_LINES_MAX {
                continue;
            }
            found
                .first
                .push(row(task, slot, tgid, task.guard.demotions));
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
        task.guard.reported = task.guard.demotions;
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
