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
}

impl State {
    pub const fn new() -> State {
        State {
            used: 0,
            home: None,
            weight: 0,
            demotions: 0,
            reported: 0,
        }
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

/// Lines for the tasks demoted since the last report, at most one per task and
/// one report per second: `SCHED:GUARD:DEMOTED task=<name> slot=<n> total=<n>`.
/// Task context only (it takes the serial lock): the multiplexer loop calls it.
pub fn report() {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = ticks();
    if now.wrapping_sub(LAST.load(Ordering::Relaxed)) < 100 || demotions() == 0 {
        return;
    }
    LAST.store(now, Ordering::Relaxed);
    let mut lines: Vec<(&'static str, usize, u32)> = Vec::new();
    {
        let mut tasks = TASKS.lock();
        for (slot, entry) in tasks.iter_mut().enumerate() {
            if let Some(task) = entry.as_mut() {
                if task.guard.demotions != task.guard.reported {
                    task.guard.reported = task.guard.demotions;
                    lines.push((task.name, slot, task.guard.demotions));
                }
            }
        }
    }
    for (name, slot, total) in lines {
        crate::serial_println!("SCHED:GUARD:DEMOTED task={name} slot={slot} total={total}");
    }
}
