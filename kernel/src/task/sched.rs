//! Priority classes and the stride scheduler (issue #58): weights, picking and deadline expiry.

use super::*;

// ---------------------------------------------------------------------------
// Priority classes and stride scheduling (issue #58)
// ---------------------------------------------------------------------------

/// Scheduling class. Classes are strictly ordered, so a runnable task in a
/// higher class always beats every task in a lower one; inside one class the
/// stride scheduler shares the CPU by weight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PriorityClass {
    /// Batch work: runs only while nothing more important is runnable.
    Background,
    /// Default for Linux and native programs.
    Normal,
    /// Latency-sensitive work: shells, editors, and the kernel multiplexer.
    Interactive,
    /// Short, latency-critical bursts (audio, input). Strictly above
    /// `Interactive`; see the module docs for the starvation trade-off.
    Realtime,
}

impl PriorityClass {
    /// Every class, lowest first (tools and tests iterate this).
    pub const ALL: [PriorityClass; 4] = [
        PriorityClass::Background,
        PriorityClass::Normal,
        PriorityClass::Interactive,
        PriorityClass::Realtime,
    ];

    /// Position in the strict priority order (higher wins).
    pub(super) const fn rank(self) -> u8 {
        match self {
            PriorityClass::Background => 0,
            PriorityClass::Normal => 1,
            PriorityClass::Interactive => 2,
            PriorityClass::Realtime => 3,
        }
    }

    /// Default weight of a task in this class. Only ratios inside one class
    /// matter (classes are strict), but the defaults grow with the class so a
    /// promoted task is also the heaviest member of its new class.
    pub const fn default_weight(self) -> u16 {
        match self {
            PriorityClass::Background => 1,
            PriorityClass::Normal => 2,
            PriorityClass::Interactive => 4,
            PriorityClass::Realtime => 8,
        }
    }

    /// Stable label for tools (`ps`, Task Manager, logs).
    #[allow(dead_code)] // used by the in-kernel tests until a `ps` tool lands
    pub const fn label(self) -> &'static str {
        match self {
            PriorityClass::Background => "background",
            PriorityClass::Normal => "normal",
            PriorityClass::Interactive => "interactive",
            PriorityClass::Realtime => "realtime",
        }
    }
}

/// Smallest assignable per-task weight.
pub const MIN_WEIGHT: u16 = 1;
/// Largest assignable per-task weight.
pub const MAX_WEIGHT: u16 = 32;
/// Virtual-time unit: a task of weight `w` pays `STRIDE_UNIT / w` of virtual
/// time per quantum, so selections follow the weight ratio.
pub(super) const STRIDE_UNIT: u64 = 1024;
/// Passes are shifted back by their minimum once it reaches this mark (only
/// ordering matters, and small passes stay far from `u64` overflow).
pub(super) const PASS_CEILING: u64 = 1 << 40;

/// The virtual time one quantum costs a task: its stride.
pub(super) fn stride(weight: u16) -> u64 {
    (STRIDE_UNIT / weight.clamp(MIN_WEIGHT, MAX_WEIGHT) as u64).max(1)
}

/// The smallest pass among runnable tasks: the scheduler's "now". A task that
/// spawns or wakes here starts even with its peers instead of claiming a
/// backlog of catch-up quanta. Walks the run queues (P6.1), not the table.
pub(super) fn virtual_now(tasks: &[Option<Task>; MAX_TASKS]) -> u64 {
    let mut now: Option<u64> = None;
    for rank in 0..PriorityClass::ALL.len() {
        for slot in runq::runnable(rank).iter() {
            if !runq::check(tasks, slot, rank) {
                continue;
            }
            if let Some(task) = tasks[slot].as_ref() {
                now = Some(now.map_or(task.pass, |now| now.min(task.pass)));
            }
        }
    }
    now.unwrap_or(0)
}

/// The smallest pass in the whole table, blocked tasks included (a blocked
/// task's pass is its place in line when it wakes). A full scan: only
/// [`renormalize`] needs it, once every [`PASS_CEILING`] of virtual time.
pub(super) fn min_pass(tasks: &[Option<Task>; MAX_TASKS]) -> u64 {
    tasks
        .iter()
        .flatten()
        .map(|task| task.pass)
        .min()
        .unwrap_or(0)
}

/// Set a task's scheduling class, resetting its weight to the class default.
/// Returns whether the slot holds a task.
///
/// This is the native/Linux-neutral priority API used by the kernel and tools;
/// Linux `nice`/`setpriority` are not routed here yet (see the module docs).
#[allow(dead_code)] // tool/test API; callers arrive with the scheduler features
pub fn set_priority(slot: usize, class: PriorityClass) -> bool {
    let mut tasks = TASKS.lock();
    match tasks.get_mut(slot).and_then(|task| task.as_mut()) {
        Some(task) => {
            task.class = class;
            task.weight = class.default_weight();
            runq::sync(&tasks, slot);
            true
        }
        None => false,
    }
}

/// Raise a task to at least `class`, never lowering it: a task already in
/// `class` or above keeps its class and weight. Returns whether the slot
/// holds a task.
pub fn raise_priority(slot: usize, class: PriorityClass) -> bool {
    let mut tasks = TASKS.lock();
    match tasks.get_mut(slot).and_then(|task| task.as_mut()) {
        Some(task) => {
            if task.class.rank() < class.rank() {
                task.class = class;
                task.weight = class.default_weight();
                runq::sync(&tasks, slot);
            }
            true
        }
        None => false,
    }
}

/// A task's scheduling class, or `None` for an empty or invalid slot.
#[allow(dead_code)] // tool/test API; callers arrive with the scheduler features
pub fn priority(slot: usize) -> Option<PriorityClass> {
    TASKS.lock().get(slot)?.as_ref().map(|task| task.class)
}

/// Set a task's weight inside its class, clamped to
/// [`MIN_WEIGHT`]..=[`MAX_WEIGHT`]. Returns whether the slot holds a task.
#[allow(dead_code)] // tool/test API; callers arrive with the scheduler features
pub fn set_weight(slot: usize, weight: u16) -> bool {
    let mut tasks = TASKS.lock();
    match tasks.get_mut(slot).and_then(|task| task.as_mut()) {
        Some(task) => {
            task.weight = weight.clamp(MIN_WEIGHT, MAX_WEIGHT);
            true
        }
        None => false,
    }
}

/// A task's weight, or `None` for an empty or invalid slot.
#[allow(dead_code)] // tool/test API; callers arrive with the scheduler features
pub fn weight(slot: usize) -> Option<u16> {
    TASKS.lock().get(slot)?.as_ref().map(|task| task.weight)
}

/// One row of [`cpu_usage`]: a task's CPU accounting and scheduling class.
#[allow(dead_code)] // tool/test API; callers arrive with the scheduler features
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuUsage {
    pub slot: usize,
    pub name: &'static str,
    pub class: PriorityClass,
    pub weight: u16,
    pub state: TaskState,
    /// CPU ticks (100 Hz) charged while this task was on the CPU.
    pub ticks: u64,
}

/// Per-task CPU accounting, oldest slot first (the kernel task included).
/// Feeds tools (`ps`, Task Manager) and future CPU quotas (issue #58).
#[allow(dead_code)] // tool/test API; callers arrive with the scheduler features
pub fn cpu_usage() -> Vec<CpuUsage> {
    let tasks = TASKS.lock();
    tasks
        .iter()
        .enumerate()
        .filter_map(|(slot, task)| {
            task.as_ref().map(|task| CpuUsage {
                slot,
                name: task.name,
                class: task.class,
                weight: task.weight,
                state: task.state,
                ticks: task.cpu_ticks,
            })
        })
        .collect()
}

/// The CPU ticks charged to `slot` (0 for an empty or invalid slot).
#[allow(dead_code)] // tool/test API; callers arrive with the scheduler features
pub fn cpu_ticks(slot: usize) -> u64 {
    TASKS
        .lock()
        .get(slot)
        .and_then(|task| task.as_ref())
        .map(|task| task.cpu_ticks)
        .unwrap_or(0)
}

/// Wake every blocked task whose absolute deadline (monotonic ns) has passed
/// at `now`, with [`WakeReason::TimedOut`], then arm the deadline timer for
/// the next one. The waiter is left enqueued on its wait queue: its wait loop
/// removes itself once it observes the reason, which keeps the queue and the
/// task table updates on their respective locks in a fixed order.
///
/// Only due entries of the timer queue are visited (P2.1). An entry whose
/// task is no longer blocked with that same deadline is stale and dropped.
pub(super) fn expire_deadlines(tasks: &mut [Option<Task>; MAX_TASKS], now: u64) {
    let cur = CURRENT.load(Ordering::Relaxed);
    let mut timers = super::timerq::TIMERS.lock();
    // Computed only if something is due: a long sleep must not earn a burst
    // of catch-up quanta, so a woken sleeper rejoins at the current virtual
    // time, like a queue wakeup (issue #58).
    let mut now_pass = None;
    while let Some((deadline, slot)) = timers.pop_due(now) {
        let current = tasks[slot].as_ref().map(|task| task.state);
        if !matches!(current, Some(TaskState::Blocked { deadline: Some(d), .. }) if d == deadline) {
            continue;
        }
        let pass = *now_pass.get_or_insert_with(|| virtual_now(tasks));
        if let Some(task) = tasks[slot].as_mut() {
            task.state = TaskState::Runnable;
            task.wake_reason = Some(WakeReason::TimedOut);
            task.pass = task.pass.max(pass);
        }
        runq::sync(tasks, slot);
        crate::perf::on_wake(slot, cur);
        super::preempt::note_wake(tasks, slot, cur);
    }
    crate::arch::event_timer::program(next_event(timers.peek()), now);
}

/// The deadline the one-shot timer must fire for, given the queue's earliest
/// entry: only one inside the current tick period. A later deadline (tick
/// deadlines included, which pass exactly when `TICKS` reaches them) is left
/// to the tick that begins its period, which re-arms the timer; nothing
/// earlier is queued. See `arch::event_timer` for why it must not be armed
/// sooner.
fn next_event(earliest: Option<(u64, usize)>) -> Option<u64> {
    let period_end = super::ticks_to_ns(super::ticks().saturating_add(1));
    earliest
        .map(|(deadline, _)| deadline)
        .filter(|&deadline| deadline < period_end)
}

/// Whether `slot` is occupied and `Runnable` (blocked and done tasks are never
/// selected).
pub(super) fn runnable(tasks: &[Option<Task>; MAX_TASKS], slot: usize) -> bool {
    tasks[slot]
        .as_ref()
        .is_some_and(|task| task.state == TaskState::Runnable)
}

/// The runnable task the stride scheduler would pick: the highest occupied
/// class, and inside it the smallest virtual pass. Ties (equal passes, e.g.
/// freshly spawned tasks) break in round-robin order after `cur`, so
/// equal-weight tasks rotate exactly like the old scheduler. Returns `None`
/// when nothing can run.
///
/// The kernel task competes like any other task. It cannot starve user work
/// because `mux::run` parks it with [`idle`] between frames: it is only
/// `Runnable` for the one quantum it needs to repaint, not all the time.
///
/// Only the class's run queue is walked (P6.1): the cost is the number of
/// runnable tasks, and the order key `(pass, distance after cur)` keeps the
/// exact choice of the full round-robin scan this replaced.
pub(super) fn pick_next_best(tasks: &[Option<Task>; MAX_TASKS], cur: usize) -> Option<usize> {
    for rank in (0..PriorityClass::ALL.len()).rev() {
        let mut best: Option<(u64, usize, usize)> = None;
        for slot in runq::runnable(rank).iter() {
            if !runq::check(tasks, slot, rank) {
                continue;
            }
            let Some(task) = tasks[slot].as_ref() else {
                continue;
            };
            // Round-robin position: `cur + 1` first, `cur` itself last.
            let order = (slot + MAX_TASKS - cur - 1) % MAX_TASKS;
            if best.is_none_or(|(pass, at, _)| (task.pass, order) < (pass, at)) {
                best = Some((task.pass, order, slot));
            }
        }
        if let Some((_, _, slot)) = best {
            return Some(slot);
        }
    }
    None
}

/// The scheduler's choice: [`pick_next_best`], or the interrupted task when
/// nothing is runnable at all so it can re-enter its wait loop instead of
/// stalling the CPU.
pub(super) fn pick_next(tasks: &[Option<Task>; MAX_TASKS], cur: usize) -> usize {
    pick_next_best(tasks, cur).unwrap_or(cur)
}

/// [`pick_next`] plus stride accounting: the selected task pays one quantum
/// (its stride) of virtual time. Only a runnable winner is charged, so a
/// degenerate fallback to a parked `cur` does not advance its pass.
pub(super) fn select_next(tasks: &mut [Option<Task>; MAX_TASKS], cur: usize) -> usize {
    // This selection accounts for every wake so far (P1.1).
    super::preempt::clear();
    let next = pick_next(tasks, cur);
    charge(tasks, next);
    next
}

/// Charge a selected runnable task one quantum (its stride) of virtual time,
/// renormalizing when its pass reaches the ceiling. Every pass is at least
/// the table minimum, so the full-table minimum can only reach
/// [`PASS_CEILING`] once the charged pass has: the scan in [`renormalize`]
/// runs then, not at every selection.
pub(super) fn charge(tasks: &mut [Option<Task>; MAX_TASKS], slot: usize) {
    if !runnable(tasks, slot) {
        return;
    }
    let mut pass = 0;
    if let Some(task) = tasks[slot].as_mut() {
        task.pass = task.pass.saturating_add(stride(task.weight));
        pass = task.pass;
    }
    if pass >= PASS_CEILING {
        renormalize(tasks);
    }
}

/// Shift every pass back by the table minimum once it reaches
/// [`PASS_CEILING`]. Passes are only ever compared, so the shift is invisible
/// to selection while keeping the virtual clock far from `u64` overflow.
pub(super) fn renormalize(tasks: &mut [Option<Task>; MAX_TASKS]) {
    let min = min_pass(tasks);
    if min < PASS_CEILING {
        return;
    }
    for task in tasks.iter_mut().flatten() {
        task.pass -= min;
    }
}
