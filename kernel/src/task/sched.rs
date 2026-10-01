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
/// backlog of catch-up quanta.
pub(super) fn virtual_now(tasks: &[Option<Task>; MAX_TASKS]) -> u64 {
    tasks
        .iter()
        .flatten()
        .filter(|task| task.state == TaskState::Runnable)
        .map(|task| task.pass)
        .min()
        .unwrap_or(0)
}

/// The smallest pass in the whole table, blocked tasks included (a blocked
/// task's pass is its place in line when it wakes).
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

/// Wake every blocked task whose absolute deadline has passed at `now`, with
/// [`WakeReason::TimedOut`]. The waiter is left enqueued: its wait loop removes
/// itself once it observes the reason, which keeps the queue and the task table
/// updates on their respective locks in a fixed order.
pub(super) fn expire_deadlines(tasks: &mut [Option<Task>; MAX_TASKS], now: u64) {
    // Waking sleepers rejoin at the current virtual time, like queue wakeups:
    // a long sleep must not earn a burst of catch-up quanta (issue #58).
    let now_pass = virtual_now(tasks);
    for task in tasks.iter_mut().flatten() {
        if let TaskState::Blocked {
            deadline: Some(deadline),
            ..
        } = task.state
        {
            if now >= deadline {
                task.state = TaskState::Runnable;
                task.wake_reason = Some(WakeReason::TimedOut);
                task.pass = task.pass.max(now_pass);
            }
        }
    }
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
pub(super) fn pick_next_best(tasks: &[Option<Task>; MAX_TASKS], cur: usize) -> Option<usize> {
    for rank in (0..PriorityClass::ALL.len()).rev() {
        let mut best: Option<(usize, u64)> = None;
        for step in 1..=MAX_TASKS {
            let slot = (cur + step) % MAX_TASKS;
            let Some(task) = tasks[slot].as_ref() else {
                continue;
            };
            if task.state != TaskState::Runnable || task.class.rank() as usize != rank {
                continue;
            }
            if best.is_none_or(|(_, pass)| task.pass < pass) {
                best = Some((slot, task.pass));
            }
        }
        if let Some((slot, _)) = best {
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
    let next = pick_next(tasks, cur);
    if runnable(tasks, next) {
        if let Some(task) = tasks[next].as_mut() {
            task.pass = task.pass.saturating_add(stride(task.weight));
        }
        renormalize(tasks);
    }
    next
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
