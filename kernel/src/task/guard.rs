//! The Interactive-class CPU guard.
//!
//! Classes are strict: a runnable `Interactive` task beats every `Normal` one,
//! so an `Interactive` task that stays busy (a compositor repainting a big
//! window on a slow CPU) starves the services and apps it is waiting on, and
//! the desktop freezes for seconds. The guard bounds that: time is cut into
//! windows of [`WINDOW_TICKS`] ticks; once the `Interactive` class has used
//! [`BUDGET_TICKS`] of one window, each `Interactive` task that runs is
//! demoted to `Normal` for the rest of the window, where it shares fairly with
//! its peers, and gets its class back at the window's end.
//!
//! The budget is the whole class's, not each task's: three tasks each under a
//! third of the CPU would otherwise still starve everything below. Tasks that
//! sleep through most of a window (input drivers, a compositor at rest) never
//! run past the budget and are never touched.
//!
//! `Interactive` work therefore holds the top class for at most 60 % of any
//! window while anything else wants the CPU, then competes as `Normal`; with
//! nothing else runnable the class makes no difference. Only `Interactive` is
//! guarded; `Realtime` is left to its callers.

use super::*;
use core::sync::atomic::{AtomicU64, Ordering};

/// Ticks (100 Hz) in one accounting window.
pub const WINDOW_TICKS: u64 = 10;
/// Ticks of a window the `Interactive` class may use, all its tasks together,
/// before the task that is running is demoted.
pub const BUDGET_TICKS: u64 = 5;

/// Per-task guard state, kept in [`Task`].
#[derive(Clone, Copy, Debug)]
pub struct State {
    /// The class to give back at the window's end while demoted.
    home: Option<PriorityClass>,
    /// Demotions since the task started.
    demotions: u32,
    /// Demotions already reported ([`report`]).
    reported: u32,
}

impl State {
    pub const fn new() -> State {
        State {
            home: None,
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
    }
}

/// Ticks into the current window.
static POSITION: AtomicU64 = AtomicU64::new(0);
/// Ticks the `Interactive` class has used this window.
static CLASS_USED: AtomicU64 = AtomicU64::new(0);
/// Demotions since boot.
static DEMOTIONS: AtomicU64 = AtomicU64::new(0);

/// Demotions since boot (the `SCHED:GUARD` report and the tests).
pub fn demotions() -> u64 {
    DEMOTIONS.load(Ordering::Relaxed)
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
    if ran {
        if let Some(task) = tasks[cur].as_mut() {
            if task.class == PriorityClass::Interactive {
                let used = CLASS_USED.fetch_add(1, Ordering::Relaxed) + 1;
                if used > BUDGET_TICKS {
                    task.guard.home = Some(PriorityClass::Interactive);
                    task.guard.demotions = task.guard.demotions.saturating_add(1);
                    task.class = PriorityClass::Normal;
                    task.weight = PriorityClass::Normal.default_weight();
                    DEMOTIONS.fetch_add(1, Ordering::Relaxed);
                    runq::sync(tasks, cur);
                }
            }
        }
    }
    let position = POSITION.fetch_add(1, Ordering::Relaxed) + 1;
    if position >= WINDOW_TICKS {
        POSITION.store(0, Ordering::Relaxed);
        CLASS_USED.store(0, Ordering::Relaxed);
        new_window(tasks);
    }
}

/// A window ended: give demoted tasks their class back.
fn new_window(tasks: &mut [Option<Task>; MAX_TASKS]) {
    for slot in 0..MAX_TASKS {
        let Some(task) = tasks[slot].as_mut() else {
            continue;
        };
        if let Some(home) = task.guard.home.take() {
            task.class = home;
            task.weight = home.default_weight();
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
