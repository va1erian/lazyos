//! Typed client for the native system-stats snapshot (issue #144), mirrored
//! from `user/src/sysinfo.rs` for the xui dashboard.
//!
//! The kernel's syscall 14 writes a fixed-layout block of little-endian `u64`
//! words: a header (version, sizes, uptime, frame/slab/heap counters) followed
//! by one row per scheduler slot. The `VERSION`/size constants and the header
//! and row indices below must stay in lock-step with `kernel/src/sysinfo.rs`
//! and `user/src/sysinfo.rs`.
//!
//! The snapshot is readable by every task and deliberately aggregate-only:
//! counters, pids, states, classes, CPU ticks and task names (see the kernel
//! module docs).

use crate::sys::{self, system_stats_op};

/// ABI version this client understands.
pub const VERSION: u64 = 3;

/// Words in the header (mirrors `kernel::sysinfo::HEADER_WORDS`).
pub const HEADER_WORDS: usize = 23;
/// Words in one task row (mirrors `kernel::sysinfo::TASK_ROW_WORDS`).
pub const TASK_ROW_WORDS: usize = 10;
/// Scheduler slots in the task table (mirrors `kernel::task::MAX_TASKS`).
pub const MAX_TASKS: usize = 256;
/// Words in the whole block.
pub const WORDS: usize = HEADER_WORDS + MAX_TASKS * TASK_ROW_WORDS;
/// Bytes in the whole block.
pub const SIZE: usize = WORDS * 8;

/// Header word indices.
pub mod header {
    /// [`super::VERSION`].
    pub const VERSION: usize = 0;
    /// PIT ticks since boot (100 Hz).
    pub const TICKS: usize = 2;
    /// Occupied slots whose state is not done.
    pub const TASKS_LIVE: usize = 3;
    /// Frames the allocator can hand out.
    pub const FRAMES_TOTAL: usize = 4;
    /// Frames currently handed out.
    pub const FRAMES_LIVE: usize = 5;
    /// Frames on the free list.
    pub const FRAMES_FREE: usize = 6;
    /// Cumulative frame allocations.
    pub const FRAMES_ALLOCATED: usize = 7;
    /// Cumulative frame frees.
    pub const FRAMES_FREED: usize = 8;
    /// Frames held for the allocator's refcount table.
    pub const FRAMES_RESERVED: usize = 9;
    /// Double frees observed.
    pub const FRAMES_DOUBLE_FREES: usize = 10;
    /// Frees of non-usable addresses observed.
    pub const FRAMES_INVALID_FREES: usize = 11;
    /// Bytes live across all slab classes.
    pub const SLAB_LIVE: usize = 12;
    /// Slab live-bytes high-water mark.
    pub const SLAB_PEAK: usize = 13;
    /// Bytes live in the heap fallback for oversized slab requests.
    pub const SLAB_OVERSIZED: usize = 14;
    /// Oversized-bytes high-water mark.
    pub const SLAB_OVERSIZED_PEAK: usize = 15;
    /// Slab frees with no live slot.
    pub const SLAB_DOUBLE_FREES: usize = 16;
    /// Failed slab owner-accounting calls.
    pub const SLAB_ACCOUNTING_ERRORS: usize = 17;
    /// Bytes the kernel heap owns.
    pub const HEAP_TOTAL: usize = 18;
    /// Bytes handed out by the kernel heap.
    pub const HEAP_USED: usize = 19;
    /// Bytes on the kernel heap's free list.
    pub const HEAP_FREE: usize = 20;
}

/// Task row word indices (relative to the row's base).
pub mod row {
    /// `1` when the slot is occupied.
    pub const PRESENT: usize = 0;
    /// Pid (the scheduler slot).
    pub const PID: usize = 1;
    /// Parent pid.
    pub const PPID: usize = 2;
    /// [`super::TaskState`] code.
    pub const STATE: usize = 3;
    /// Wait-kind code when blocked.
    pub const WAIT: usize = 4;
    /// [`super::TaskClass`] code.
    pub const CLASS: usize = 5;
    /// Weight inside the class.
    pub const WEIGHT: usize = 6;
    /// CPU ticks (100 Hz) charged to the task.
    pub const CPU_TICKS: usize = 7;
    /// `fnv1a64` hash of the full task name.
    pub const NAME_HASH: usize = 8;
    /// Up to eight name bytes, little-endian and NUL-padded.
    pub const NAME8: usize = 9;
}

/// A task's scheduler-visible state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskState {
    /// Eligible for the scheduler.
    Runnable,
    /// Parked on a wait queue.
    Blocked,
    /// Finished, waiting to be reaped.
    Done,
    /// A code this client does not know.
    Unknown,
}

impl TaskState {
    /// Decode a row's state word.
    pub fn from_code(code: u64) -> TaskState {
        match code {
            0 => TaskState::Runnable,
            1 => TaskState::Blocked,
            2 => TaskState::Done,
            _ => TaskState::Unknown,
        }
    }

    /// A short label for tables.
    pub fn label(self) -> &'static str {
        match self {
            TaskState::Runnable => "run",
            TaskState::Blocked => "block",
            TaskState::Done => "done",
            TaskState::Unknown => "?",
        }
    }
}

/// A task's scheduling class.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskClass {
    /// Batch work.
    Background,
    /// Default for Linux and native programs.
    Normal,
    /// Latency-sensitive work.
    Interactive,
    /// Short, latency-critical bursts.
    Realtime,
    /// A code this client does not know.
    Unknown,
}

impl TaskClass {
    /// Decode a row's class word.
    pub fn from_code(code: u64) -> TaskClass {
        match code {
            0 => TaskClass::Background,
            1 => TaskClass::Normal,
            2 => TaskClass::Interactive,
            3 => TaskClass::Realtime,
            _ => TaskClass::Unknown,
        }
    }

    /// A short label for tables.
    pub fn label(self) -> &'static str {
        match self {
            TaskClass::Background => "bg",
            TaskClass::Normal => "norm",
            TaskClass::Interactive => "intr",
            TaskClass::Realtime => "rt",
            TaskClass::Unknown => "?",
        }
    }
}

/// One task-table row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TaskRow {
    /// Whether the slot is occupied.
    pub present: bool,
    /// Pid (the scheduler slot).
    pub pid: u64,
    /// Parent pid.
    pub ppid: u64,
    /// Scheduler-visible state.
    pub state: TaskState,
    /// Scheduling class.
    pub class: TaskClass,
    /// Weight inside the class.
    pub weight: u64,
    /// CPU ticks (100 Hz) charged to the task.
    pub cpu_ticks: u64,
    /// Up to eight name bytes, NUL-padded.
    pub short_name: [u8; 8],
}

impl TaskRow {
    /// A row for an empty slot.
    pub const EMPTY: TaskRow = TaskRow {
        present: false,
        pid: 0,
        ppid: 0,
        state: TaskState::Done,
        class: TaskClass::Normal,
        weight: 0,
        cpu_ticks: 0,
        short_name: [0; 8],
    };

    /// Whether the slot holds a task that is not done.
    pub fn live(&self) -> bool {
        self.present && self.state != TaskState::Done
    }

    /// The NUL-trimmed short name (at most eight bytes, so it may be a prefix).
    pub fn name(&self) -> &str {
        let end = self
            .short_name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(self.short_name.len());
        core::str::from_utf8(&self.short_name[..end]).unwrap_or("")
    }
}

/// A decoded system-stats snapshot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Snapshot {
    /// ABI version of the block.
    pub version: u64,
    /// PIT ticks since boot (100 Hz).
    pub ticks: u64,
    /// Occupied slots whose state is not done.
    pub tasks_live: u64,
    /// Frames the allocator can hand out.
    pub frames_total: u64,
    /// Frames currently handed out.
    pub frames_live: u64,
    /// Frames on the free list.
    pub frames_free: u64,
    /// Cumulative frame allocations.
    pub frames_allocated: u64,
    /// Cumulative frame frees.
    pub frames_freed: u64,
    /// Frames held for the allocator's refcount table.
    pub frames_reserved: u64,
    /// Double frees observed (should stay zero).
    pub frames_double_frees: u64,
    /// Frees of non-usable addresses observed (should stay zero).
    pub frames_invalid_frees: u64,
    /// Bytes live across all slab classes.
    pub slab_live: u64,
    /// Slab live-bytes high-water mark.
    pub slab_peak: u64,
    /// Bytes live in the heap fallback for oversized slab requests.
    pub slab_oversized: u64,
    /// Oversized-bytes high-water mark.
    pub slab_oversized_peak: u64,
    /// Slab frees with no live slot (should stay zero).
    pub slab_double_frees: u64,
    /// Failed slab owner-accounting calls (should stay zero).
    pub slab_accounting_errors: u64,
    /// Bytes the kernel heap owns.
    pub heap_total: u64,
    /// Bytes handed out by the kernel heap.
    pub heap_used: u64,
    /// Bytes on the kernel heap's free list.
    pub heap_free: u64,
    /// One row per scheduler slot.
    pub tasks: [TaskRow; MAX_TASKS],
}

impl Snapshot {
    /// The live task rows, oldest slot first.
    pub fn live_tasks(&self) -> impl Iterator<Item = &TaskRow> {
        self.tasks.iter().filter(|row| row.live())
    }
}

/// Decode a block from raw words. Returns `None` when the version is not
/// [`VERSION`].
pub fn decode_words(words: &[u64; WORDS]) -> Option<Snapshot> {
    if words[header::VERSION] != VERSION {
        return None;
    }
    let mut tasks = [TaskRow::EMPTY; MAX_TASKS];
    for (slot, row) in tasks.iter_mut().enumerate() {
        let base = HEADER_WORDS + slot * TASK_ROW_WORDS;
        if words[base + row::PRESENT] == 0 {
            continue;
        }
        *row = TaskRow {
            present: true,
            pid: words[base + row::PID],
            ppid: words[base + row::PPID],
            state: TaskState::from_code(words[base + row::STATE]),
            class: TaskClass::from_code(words[base + row::CLASS]),
            weight: words[base + row::WEIGHT],
            cpu_ticks: words[base + row::CPU_TICKS],
            short_name: words[base + row::NAME8].to_le_bytes(),
        };
    }
    let word = |index: usize| words[index];
    Some(Snapshot {
        version: word(header::VERSION),
        ticks: word(header::TICKS),
        tasks_live: word(header::TASKS_LIVE),
        frames_total: word(header::FRAMES_TOTAL),
        frames_live: word(header::FRAMES_LIVE),
        frames_free: word(header::FRAMES_FREE),
        frames_allocated: word(header::FRAMES_ALLOCATED),
        frames_freed: word(header::FRAMES_FREED),
        frames_reserved: word(header::FRAMES_RESERVED),
        frames_double_frees: word(header::FRAMES_DOUBLE_FREES),
        frames_invalid_frees: word(header::FRAMES_INVALID_FREES),
        slab_live: word(header::SLAB_LIVE),
        slab_peak: word(header::SLAB_PEAK),
        slab_oversized: word(header::SLAB_OVERSIZED),
        slab_oversized_peak: word(header::SLAB_OVERSIZED_PEAK),
        slab_double_frees: word(header::SLAB_DOUBLE_FREES),
        slab_accounting_errors: word(header::SLAB_ACCOUNTING_ERRORS),
        heap_total: word(header::HEAP_TOTAL),
        heap_used: word(header::HEAP_USED),
        heap_free: word(header::HEAP_FREE),
        tasks,
    })
}

/// Read one snapshot through native syscall 14.
///
/// Checks the kernel-reported size against this client's [`SIZE`] first, so a
/// layout change is a clean `-EINVAL` instead of a decode of garbage. Returns
/// the negative errno on failure.
pub fn snapshot() -> Result<Snapshot, i64> {
    const EINVAL: i64 = 22;
    let reported = sys::system_stats(system_stats_op::SIZE, 0, 0);
    if reported < 0 {
        return Err(reported);
    }
    if reported as usize != SIZE {
        return Err(-EINVAL);
    }
    let mut words = [0u64; WORDS];
    let code = sys::system_stats(
        system_stats_op::SNAPSHOT,
        words.as_mut_ptr() as u64,
        SIZE as u64,
    );
    if code < 0 {
        return Err(code);
    }
    decode_words(&words).ok_or(-EINVAL)
}
