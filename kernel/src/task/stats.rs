//! Liveness, snapshots and task statistics.

use super::*;

/// Whether `index` holds a task that is alive (occupied and not finished).
///
/// The display grant uses this to tell a bound compositor apart from a dead
/// one, so the kernel mux can take the screen back without a teardown hook
/// (issue #113). Cheap: one table lock and no allocation.
///
/// Interrupts are disabled around the lock: the kernel mux calls this every
/// frame with interrupts enabled (via `display::bound`), and the timer ISR's
/// `schedule` (and the keyboard/mouse IRQ handlers, through
/// `display::bound`) take this same lock. A timer tick landing inside the
/// critical section would spin in the ISR forever on a single CPU. TCG
/// only delivers interrupts at translation-block boundaries, which hid the
/// window; under KVM it hung the xui sysmon boot within ~100 s.
pub fn live(index: usize) -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| {
        #[cfg(lazyos_tests)]
        harness::note_critical_section();
        TASKS.lock().get(index).is_some_and(|task| {
            task.as_ref()
                .is_some_and(|task| task.state != TaskState::Done)
        })
    })
}

/// Snapshot of a task's name, output and done flag, for rendering.
pub fn snapshot(index: usize) -> Option<(&'static str, Vec<u8>, bool)> {
    let tasks = TASKS.lock();
    tasks[index].as_ref().map(|task| {
        (
            task.name,
            task.output.clone(),
            task.state == TaskState::Done,
        )
    })
}

/// One row of [`stats_snapshot`] (issue #144): the task-table view the
/// system-stats syscall copies into its fixed ABI layout. It carries no
/// addresses or credentials, so it is safe to hand to any task.
#[derive(Clone, Copy)]
pub struct StatsRow {
    /// Whether the slot is occupied (a `Done` zombie still counts).
    pub present: bool,
    /// Pid (the slot, see [`process`]).
    pub pid: usize,
    /// Parent pid; `0` is the kernel/init task.
    pub ppid: usize,
    /// Scheduler-visible state.
    pub state: TaskState,
    /// Scheduling class.
    pub class: PriorityClass,
    /// Weight inside the class.
    pub weight: u16,
    /// CPU ticks (100 Hz) charged to this task.
    pub cpu_ticks: u64,
    /// Task name (already interned to `'static`).
    pub name: &'static str,
}

impl StatsRow {
    /// Placeholder for an empty slot; `present` is false.
    const EMPTY: StatsRow = StatsRow {
        present: false,
        pid: 0,
        ppid: 0,
        state: TaskState::Done,
        class: PriorityClass::Normal,
        weight: 0,
        cpu_ticks: 0,
        name: "",
    };
}

/// A whole-table task snapshot for the system-stats syscall (issue #144).
pub struct TaskStats {
    /// One row per scheduler slot (empty slots are `present == false`).
    pub rows: Vec<StatsRow>,
    /// Occupied slots whose state is not `Done`.
    pub live: usize,
}

/// Snapshot the task table (one row per slot, no allocation). Takes only the
/// task-table lock, so it can never nest inside another subsystem's lock.
pub fn stats_snapshot() -> TaskStats {
    let tasks = TASKS.lock();
    let mut snapshot = TaskStats {
        // On the heap: 256 rows would take over half of a kernel stack.
        rows: alloc::vec![StatsRow::EMPTY; MAX_TASKS],
        live: 0,
    };
    for (slot, task) in tasks.iter().enumerate() {
        let Some(task) = task else { continue };
        snapshot.rows[slot] = StatsRow {
            present: true,
            pid: slot,
            ppid: task.parent,
            state: task.state,
            class: task.class,
            weight: task.weight,
            cpu_ticks: task.cpu_ticks,
            name: task.name,
        };
        if task.state != TaskState::Done {
            snapshot.live += 1;
        }
    }
    snapshot
}
