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
            ppid: super::family::ppid(&tasks, slot),
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

/// A live user address space and the tasks that run in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceOwner {
    /// The process: the thread-group leader's slot, what `getpid` answers in
    /// every thread of it.
    pub pid: usize,
    /// The space's PML4.
    pub pml4: u64,
    /// How many live tasks (threads) run in it.
    pub threads: usize,
}

/// The user address spaces tasks are running in, one entry each, in the order
/// seen. Kernel tasks (on the boot table) and finished tasks have none.
/// Takes only the task-table lock.
pub fn space_owners() -> Vec<SpaceOwner> {
    // The boot table, not `kernel_table()`: that one is whatever table is active, which
    // during a syscall is the caller's own.
    let kernel = mem::boot_table().as_u64();
    let tasks = TASKS.lock();
    let mut owners: Vec<SpaceOwner> = Vec::new();
    for (slot, task) in tasks.iter().enumerate() {
        let Some(task) = task else { continue };
        if task.state == TaskState::Done || task.pml4 == kernel || task.pml4 == 0 {
            continue;
        }
        match owners.iter_mut().find(|owner| owner.pml4 == task.pml4) {
            Some(owner) => owner.threads += 1,
            None => owners.push(SpaceOwner {
                pid: super::family::leader_of(&tasks, slot),
                pml4: task.pml4,
                threads: 1,
            }),
        }
    }
    owners
}

/// The machine's idle time in millionths of a tick. A tick that finds a CPU
/// idle (`schedule::charge_tick`) adds that CPU's share, a million over the
/// CPUs that may run tasks then (S8.3c: every scheduling CPU ticks), so
/// `ticks() - idle_ticks()` is busy time however many CPUs tick, and a
/// change in their number does not re-weigh the past.
static IDLE_SHARE: AtomicU64 = AtomicU64::new(0);

/// One tick's worth of [`IDLE_SHARE`].
const SHARE_PER_TICK: u64 = 1_000_000;

/// `ticks` timer ticks found this CPU idle.
pub(super) fn note_idle_ticks(ticks: u64) {
    let share = SHARE_PER_TICK / super::idle::running_cpus() as u64;
    IDLE_SHARE.fetch_add(ticks.saturating_mul(share), Ordering::Relaxed);
}

/// The machine's idle time in ticks.
pub fn idle_ticks() -> u64 {
    IDLE_SHARE.load(Ordering::Relaxed) / SHARE_PER_TICK
}

/// Set the idle time back to `ticks` (the suite's simulated ticks are not
/// time that passed, and uptime accounting compares the two).
#[cfg(lazyos_tests)]
pub fn set_idle_ticks(ticks: u64) {
    IDLE_SHARE.store(ticks.saturating_mul(SHARE_PER_TICK), Ordering::Relaxed);
}

/// The raw share, for a save and restore that loses nothing.
#[cfg(lazyos_tests)]
pub(super) fn idle_share() -> u64 {
    IDLE_SHARE.load(Ordering::Relaxed)
}

/// Restore a share [`idle_share`] read.
#[cfg(lazyos_tests)]
pub(super) fn set_idle_share(share: u64) {
    IDLE_SHARE.store(share, Ordering::Relaxed);
}
