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
//!   (the budget is the class's, so several small tasks are bounded too) -
//!   except the input path: a task marked by [`mark_input`] (the input
//!   consumer and the drivers that publish to it) runs its first
//!   [`INPUT_SHARE_TICKS`] of a window outside the class's budget, from a
//!   pool of [`INPUT_POOL_TICKS`] a window, so a heavy compositor frame
//!   does not take the input driver down with it (issue #860);
//! * a `Normal` *process* that has run [`NORMAL_BUDGET_TICKS`] of a window,
//!   all its threads together (one booking per process, `BOOKINGS`), has every
//!   runnable thread demoted to `Background` for the rest of it (a thread
//!   asleep at that moment joins on its first tick after waking): every other `Normal` task now outranks them, while they
//!   still get the CPU whenever nothing else wants it, so an otherwise idle
//!   machine loses nothing. Counting per thread let a process with N busy
//!   threads take N times the CPU of a single-threaded peer.
//!
//! Ticks taken inside interrupt windows (`arch::irq_window`) and ticks that
//! land on a finished or parked task are handled by `schedule::charge_tick`
//! and `schedule::charge_window_ticks`: the first reach the guard like any
//! other, the second never do.
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
/// Ticks of a window one `Normal` process (all its threads together) may use
/// before it is demoted.
pub const NORMAL_BUDGET_TICKS: u8 = 6;
/// Ticks of a window a task marked by [`mark_input`] may run outside the
/// `Interactive` budget: a keystroke or a pointer move is a tick or two.
pub const INPUT_SHARE_TICKS: u8 = 3;
/// Ticks of a window all input tasks together may run outside the budget; past
/// it (or past their own share) they are charged like any `Interactive` task.
pub const INPUT_POOL_TICKS: u64 = 3;

/// Per-task guard state, kept in [`Task`].
#[derive(Clone, Copy, Debug)]
pub struct State {
    /// Ticks run this window while `Normal`.
    used: u8,
    /// Ticks run this window while `Interactive`.
    interactive: u8,
    /// On the input path ([`mark_input`]).
    input: bool,
    /// The class to give back at the window's end while demoted.
    home: Option<PriorityClass>,
    /// The weight to give back with it (a configured one, `set_weight`).
    weight: u16,
    /// Demotions since the task started.
    demotions: u32,
    /// Demotions already reported ([`report`]).
    reported: u32,
    /// The process was already named by a first-demotion line: this task's
    /// demotions wait for the process's next summary.
    quiet: bool,
    /// Timer ticks that found the task in ring 3 (`PERF:CPU`'s user share).
    user_ticks: u64,
}

impl State {
    pub const fn new() -> State {
        State {
            used: 0,
            interactive: 0,
            input: false,
            home: None,
            weight: 0,
            demotions: 0,
            reported: 0,
            quiet: false,
            user_ticks: 0,
        }
    }

    /// The task is no longer on the input path (its source or consumer was
    /// closed, or it ran another program).
    pub fn clear_input(&mut self) {
        self.input = false;
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
        self.interactive = 0;
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
/// Ticks the input path has used outside the budget this window.
static INPUT_USED: AtomicU64 = AtomicU64::new(0);
/// Demotions since boot (a process demoted as a whole counts once).
static DEMOTIONS: AtomicU64 = AtomicU64::new(0);

/// Processes the per-window ledger follows; one more falls back to the
/// per-thread count of the old guard for that window.
const LEDGER: usize = 32;

/// One process's ticks this window, kept so charging a tick is one lookup
/// instead of a walk over the task table with interrupts off (issue #860).
struct Booking {
    /// The address space (0: free). Set once per window per process.
    pml4: AtomicU64,
    used: AtomicU64,
    /// The process went over its budget and was demoted this window.
    over: AtomicBool,
}

static BOOKINGS: [Booking; LEDGER] = [const {
    Booking {
        pml4: AtomicU64::new(0),
        used: AtomicU64::new(0),
        over: AtomicBool::new(false),
    }
}; LEDGER];

/// Book one tick to process `pml4`; its ticks this window and whether it has
/// already been demoted this window, or `None` if the ledger is full.
fn book(pml4: u64) -> Option<(u64, bool)> {
    let mut free = None;
    for entry in &BOOKINGS {
        match entry.pml4.load(Ordering::Relaxed) {
            key if key == pml4 => {
                let used = entry.used.fetch_add(1, Ordering::Relaxed) + 1;
                return Some((used, entry.over.load(Ordering::Relaxed)));
            }
            0 if free.is_none() => free = Some(entry),
            _ => {}
        }
    }
    let entry = free?;
    entry.pml4.store(pml4, Ordering::Relaxed);
    entry.used.store(1, Ordering::Relaxed);
    entry.over.store(false, Ordering::Relaxed);
    Some((1, false))
}

/// Mark process `pml4` demoted for this window.
fn mark_over(pml4: u64) {
    if let Some(entry) = BOOKINGS
        .iter()
        .find(|e| e.pml4.load(Ordering::Relaxed) == pml4)
    {
        entry.over.store(true, Ordering::Relaxed);
    }
}

/// Forget every booking (a window ended).
fn clear_bookings() {
    for entry in &BOOKINGS {
        entry.pml4.store(0, Ordering::Relaxed);
        entry.used.store(0, Ordering::Relaxed);
        entry.over.store(false, Ordering::Relaxed);
    }
}
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
    INPUT_USED.store(0, Ordering::Relaxed);
    clear_bookings();
}

/// Ticks into the current window (the replay tests).
#[cfg(lazyos_tests)]
pub fn position_for_test() -> u64 {
    POSITION.load(Ordering::Relaxed)
}

/// Demotions task `slot` has had since it started (the suite).
#[cfg(lazyos_tests)]
pub fn task_demotions_for_test(slot: usize) -> u32 {
    TASKS.lock()[slot]
        .as_ref()
        .map_or(0, |task| task.guard.demotions)
}

/// Start the window `ticks` ticks in (the replay tests start anywhere).
#[cfg(lazyos_tests)]
pub fn set_position_for_test(ticks: u64) {
    reset_window();
    POSITION.store(ticks, Ordering::Relaxed);
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
        INPUT_USED.store(0, Ordering::Relaxed);
        clear_bookings();
        new_window(tasks);
    }
}

/// `ticks` timer ticks were taken inside interrupt windows (a long syscall
/// with interrupts off) and belong to `cur`: charge them like ordinary ticks,
/// without letting the replay cross more than one window boundary. The
/// ticks that fit in the current window are booked to it; if it ends, the
/// rest (at most a window less one) are booked to the fresh one, so a task
/// that ran a whole window inside a syscall ends it charged for a whole
/// window, not for the 0-9 ticks the replay left after rolling windows.
pub(super) fn on_window_ticks(tasks: &mut [Option<Task>; MAX_TASKS], cur: usize, ticks: u64) {
    if !ENABLED.load(Ordering::Relaxed) || ticks == 0 {
        return;
    }
    let remaining = WINDOW_TICKS - POSITION.load(Ordering::Relaxed).min(WINDOW_TICKS - 1);
    let now = ticks.min(remaining);
    for _ in 0..now {
        on_tick(tasks, cur, true);
    }
    for _ in 0..(ticks - now).min(WINDOW_TICKS - 1) {
        on_tick(tasks, cur, true);
    }
}

/// Book one run tick to `cur` and demote what is past a budget.
///
/// * `Interactive`: the class has [`BUDGET_TICKS`] a window, all its tasks
///   together. Once they are used, the task that is running is demoted,
///   unless it is on the input path and within its [`INPUT_SHARE_TICKS`]:
///   the input driver must not drop out because the compositor, compositing
///   a big frame, spent the budget (issue #860).
/// * `Normal`: the budget is the *process's*, all its threads together
///   ([`NORMAL_BUDGET_TICKS`]); past it every `Normal` thread of the process is
///   demoted, so a process with many busy threads cannot take more CPU than
///   one with a single thread.
fn charge(tasks: &mut [Option<Task>; MAX_TASKS], cur: usize) {
    let Some(task) = tasks[cur].as_mut() else {
        return;
    };
    let class = task.class;
    match class {
        PriorityClass::Interactive => {
            task.guard.interactive = task.guard.interactive.saturating_add(1);
            if task.guard.input
                && task.guard.interactive <= INPUT_SHARE_TICKS
                && INPUT_USED.fetch_add(1, Ordering::Relaxed) < INPUT_POOL_TICKS
            {
                return; // the input path's own share: not the class's
            }
            let used = CLASS_USED.fetch_add(1, Ordering::Relaxed) + 1;
            if used > BUDGET_TICKS {
                demote(tasks, cur, PriorityClass::Normal, true);
            }
        }
        PriorityClass::Normal => {
            task.guard.used = task.guard.used.saturating_add(1);
            let pml4 = task.pml4;
            let budget = u64::from(NORMAL_BUDGET_TICKS);
            let (over, already) = match book(pml4) {
                Some((used, already)) => (used > budget, already),
                // Ledger full: this thread's own count, as before the ledger.
                None => (u64::from(task.guard.used) > budget, false),
            };
            if already {
                // The process was demoted this window and this thread was
                // asleep for it: it runs, so it joins the demotion.
                demote(tasks, cur, PriorityClass::Background, false);
            } else if over {
                demote_process(tasks, cur, pml4);
            }
        }
        _ => {}
    }
}

/// Demote the **runnable** `Normal` threads of `pml4` to `Background` for the
/// window: `cur` (which ran the tick that went over) and the threads that
/// compete for the CPU. A thread asleep or finished is left alone: it holds no
/// CPU, and one that wakes is demoted by its first tick (`charge`). The
/// process counts as one demotion.
fn demote_process(tasks: &mut [Option<Task>; MAX_TASKS], cur: usize, pml4: u64) {
    mark_over(pml4);
    DEMOTIONS.fetch_add(1, Ordering::Relaxed);
    for slot in 0..MAX_TASKS {
        let Some(task) = tasks[slot].as_ref() else {
            continue;
        };
        let competes = slot == cur || task.state == TaskState::Runnable;
        if task.pml4 == pml4 && task.class == PriorityClass::Normal && competes {
            demote(tasks, slot, PriorityClass::Background, false);
        }
    }
}

/// Move `slot` down to `target` until the window ends; `count` it as a
/// demotion of its own.
fn demote(tasks: &mut [Option<Task>; MAX_TASKS], slot: usize, target: PriorityClass, count: bool) {
    let Some(task) = tasks[slot].as_mut() else {
        return;
    };
    task.guard.demote(task.class, task.weight);
    task.class = target;
    task.weight = target.default_weight();
    if count {
        DEMOTIONS.fetch_add(1, Ordering::Relaxed);
    }
    runq::sync(tasks, slot);
}

/// A window ended: clear every budget and give demoted tasks their class back.
fn new_window(tasks: &mut [Option<Task>; MAX_TASKS]) {
    for slot in 0..MAX_TASKS {
        let Some(task) = tasks[slot].as_mut() else {
            continue;
        };
        task.guard.used = 0;
        task.guard.interactive = 0;
        if let Some(home) = task.guard.home.take() {
            task.class = home;
            task.weight = task.guard.weight;
            runq::sync(tasks, slot);
        }
    }
}

/// Mark `slot` as on the input path: the input consumer, or a driver that
/// publishes to it (`input::rawsys`). Its first few ticks of every window do
/// not count against the `Interactive` class, so a compositor spending the
/// class's budget on a big frame does not demote it with itself.
pub fn mark_input(slot: usize) {
    if let Some(task) = TASKS.lock()[slot].as_mut() {
        task.guard.input = true;
    }
}

/// `slot` closed its input source or consumer: it is an ordinary task again.
pub fn unmark_input(slot: usize) {
    if let Some(task) = TASKS.lock()[slot].as_mut() {
        task.guard.clear_input();
    }
}

mod report;
#[cfg(lazyos_tests)]
pub use report::collect_for_test;
pub use report::report;
