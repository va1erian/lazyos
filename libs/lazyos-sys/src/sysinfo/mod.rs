//! Typed client for the native system-stats snapshot (issue #144), shared by
//! the native `top`/`sysmond` and the xui system monitor and widget.
//!
//! The kernel's syscall 14 writes a fixed-layout block of little-endian `u64`
//! words: a header (version, sizes, uptime, frame/slab/heap counters) followed
//! by one row per scheduler slot. This module mirrors that layout (the field
//! and index constants below must stay in lock-step with
//! `kernel/src/sysinfo.rs`), [decodes](Snapshot) it, and exposes
//! [`snapshot`] plus the memory/task views `top` and `sysmond` render.
//!
//! The same layout travels over Messenger: `sysmond`'s `snapshot` reply
//! carries the raw words, and [`decode_bytes`] decodes them without caring
//! whether they came from the syscall or a parcel.
//!
//! Permission: the snapshot is readable by every task (see the kernel module
//! docs); it contains no addresses or credentials, only counters, pids,
//! states, classes, CPU ticks and names.

use alloc::vec;
use alloc::vec::Vec;

use crate::stats;

mod cpu;
mod memory;

pub use cpu::{cpu_percent, CpuSample};
pub use memory::{MemoryUse, PAGE};

/// ABI version this client understands (5: the cache and slab frame counts;
/// 4 added the idle tick counter, 3 had 256 rows, 2 had 64, issue #204).
pub const VERSION: u64 = 5;

/// Words in the header (mirrors `kernel::sysinfo::HEADER_WORDS`).
pub const HEADER_WORDS: usize = 26;
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
    /// Words in the whole block.
    pub const WORDS: usize = 1;
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
    /// Words per task row.
    pub const TASK_ROW_WORDS: usize = 21;
    /// Task rows following the header.
    pub const TASK_SLOTS: usize = 22;
    /// PIT ticks that found the CPU idle (no task runnable).
    pub const IDLE_TICKS: usize = 23;
    /// Frames the block caches hold (part of the live frames).
    pub const CACHE_FRAMES: usize = 24;
    /// Frames carved into typed-object slabs (part of the live frames).
    pub const SLAB_FRAMES: usize = 25;
}

/// Task row word indices (relative to a row's base).
pub mod row {
    /// `1` when the slot is occupied (a done zombie still counts).
    pub const PRESENT: usize = 0;
    /// Pid (the scheduler slot).
    pub const PID: usize = 1;
    /// Parent pid; `0` is the kernel/init task.
    pub const PPID: usize = 2;
    /// [`super::TaskState`] code.
    pub const STATE: usize = 3;
    /// [`super::WaitKind`] code when the state is blocked.
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
    /// Parked on a wait queue; see [`TaskRow::wait`].
    Blocked,
    /// Finished, waiting to be reaped or reclaimed.
    Done,
    /// A code this client does not know (a newer kernel).
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

/// Why a blocked task is parked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WaitKind {
    /// Not blocked.
    None,
    /// A futex word.
    Futex,
    /// Terminal input.
    Terminal,
    /// A child exit (`wait4`).
    ChildExit,
    /// A sleep deadline.
    Sleep,
    /// A pipe or socket event.
    Pipe,
    /// A `poll` set.
    Poll,
    /// A stop signal.
    Signal,
    /// A free task slot (`clone` pressure).
    Slot,
    /// A code this client does not know.
    Unknown,
}

impl WaitKind {
    /// Decode a row's wait word.
    pub fn from_code(code: u64) -> WaitKind {
        match code {
            0 => WaitKind::None,
            1 => WaitKind::Futex,
            2 => WaitKind::Terminal,
            3 => WaitKind::ChildExit,
            4 => WaitKind::Sleep,
            5 => WaitKind::Pipe,
            6 => WaitKind::Poll,
            7 => WaitKind::Signal,
            8 => WaitKind::Slot,
            _ => WaitKind::Unknown,
        }
    }

    /// A short label for tables.
    pub fn label(self) -> &'static str {
        match self {
            WaitKind::None => "",
            WaitKind::Futex => "futex",
            WaitKind::Terminal => "tty",
            WaitKind::ChildExit => "child",
            WaitKind::Sleep => "sleep",
            WaitKind::Pipe => "pipe",
            WaitKind::Poll => "poll",
            WaitKind::Signal => "sig",
            WaitKind::Slot => "slot",
            WaitKind::Unknown => "?",
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
    /// Latency-sensitive work (shells, editors, the mux).
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
    /// Whether the slot is occupied (a done zombie still counts).
    pub present: bool,
    /// Pid (the scheduler slot).
    pub pid: u64,
    /// Parent pid; `0` is the kernel/init task.
    pub ppid: u64,
    /// Scheduler-visible state.
    pub state: TaskState,
    /// Why the task is blocked (when it is).
    pub wait: WaitKind,
    /// Scheduling class.
    pub class: TaskClass,
    /// Weight inside the class.
    pub weight: u64,
    /// CPU ticks (100 Hz) charged to the task.
    pub cpu_ticks: u64,
    /// `fnv1a64` hash of the full task name.
    pub name_hash: u64,
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
        wait: WaitKind::None,
        class: TaskClass::Normal,
        weight: 0,
        cpu_ticks: 0,
        name_hash: 0,
        short_name: [0; 8],
    };

    /// Whether the slot holds a task that is not done.
    pub fn live(&self) -> bool {
        self.present && self.state != TaskState::Done
    }

    /// The NUL-trimmed short name (at most eight bytes, so it may be a
    /// prefix; [`TaskRow::name_hash`] identifies the full name).
    pub fn name(&self) -> &str {
        let end = self
            .short_name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(self.short_name.len());
        core::str::from_utf8(&self.short_name[..end]).unwrap_or("")
    }
}

/// A decoded system-stats snapshot. The per-slot rows and the raw words live on
/// the heap: at 256 slots they would be ~40 KiB, a third of a user stack.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Snapshot {
    /// ABI version of the block.
    pub version: u64,
    /// Words in the block (should equal [`WORDS`]).
    pub words: u64,
    /// PIT ticks since boot (100 Hz).
    pub ticks: u64,
    /// PIT ticks that found the CPU idle (no task runnable).
    pub idle_ticks: u64,
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
    /// Double frees observed.
    pub frames_double_frees: u64,
    /// Frees of non-usable addresses observed.
    pub frames_invalid_frees: u64,
    /// Bytes live across all slab classes.
    pub slab_live: u64,
    /// Slab live-bytes high-water mark.
    pub slab_peak: u64,
    /// Bytes live in the heap fallback for oversized slab requests.
    pub slab_oversized: u64,
    /// Oversized-bytes high-water mark.
    pub slab_oversized_peak: u64,
    /// Slab frees with no live slot.
    pub slab_double_frees: u64,
    /// Failed slab owner-accounting calls.
    pub slab_accounting_errors: u64,
    /// Bytes the kernel heap owns.
    pub heap_total: u64,
    /// Bytes handed out by the kernel heap.
    pub heap_used: u64,
    /// Bytes on the kernel heap's free list.
    pub heap_free: u64,
    /// Frames the block caches hold (part of [`Snapshot::frames_live`]).
    pub cache_frames: u64,
    /// Frames carved into typed-object slabs (part of
    /// [`Snapshot::frames_live`]).
    pub slab_frames: u64,
    /// One row per scheduler slot.
    pub tasks: Vec<TaskRow>,
    /// The raw words, so a snapshot can be re-encoded for the wire.
    raw: Vec<u64>,
}

impl Snapshot {
    /// The raw little-endian words, exactly as the kernel wrote them.
    pub fn raw_words(&self) -> &[u64] {
        &self.raw
    }

    /// Copy the raw words into `out` as bytes; `false` when `out` is too
    /// small.
    pub fn write_bytes(&self, out: &mut [u8]) -> bool {
        if out.len() < SIZE {
            return false;
        }
        for (index, word) in self.raw.iter().enumerate() {
            out[index * 8..index * 8 + 8].copy_from_slice(&word.to_le_bytes());
        }
        true
    }

    /// The live task rows, oldest slot first.
    pub fn live_tasks(&self) -> impl Iterator<Item = &TaskRow> {
        self.tasks.iter().filter(|row| row.live())
    }
}

/// Decode a block from raw words. Returns `None` when the version is not
/// [`VERSION`].
pub fn decode_words(words: &[u64]) -> Option<Snapshot> {
    if words.len() < WORDS || words[header::VERSION] != VERSION {
        return None;
    }
    let mut tasks = vec![TaskRow::EMPTY; MAX_TASKS];
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
            wait: WaitKind::from_code(words[base + row::WAIT]),
            class: TaskClass::from_code(words[base + row::CLASS]),
            weight: words[base + row::WEIGHT],
            cpu_ticks: words[base + row::CPU_TICKS],
            name_hash: words[base + row::NAME_HASH],
            short_name: words[base + row::NAME8].to_le_bytes(),
        };
    }
    Some(Snapshot {
        version: words[header::VERSION],
        words: words[header::WORDS],
        ticks: words[header::TICKS],
        idle_ticks: words[header::IDLE_TICKS],
        tasks_live: words[header::TASKS_LIVE],
        frames_total: words[header::FRAMES_TOTAL],
        frames_live: words[header::FRAMES_LIVE],
        frames_free: words[header::FRAMES_FREE],
        frames_allocated: words[header::FRAMES_ALLOCATED],
        frames_freed: words[header::FRAMES_FREED],
        frames_reserved: words[header::FRAMES_RESERVED],
        frames_double_frees: words[header::FRAMES_DOUBLE_FREES],
        frames_invalid_frees: words[header::FRAMES_INVALID_FREES],
        slab_live: words[header::SLAB_LIVE],
        slab_peak: words[header::SLAB_PEAK],
        slab_oversized: words[header::SLAB_OVERSIZED],
        slab_oversized_peak: words[header::SLAB_OVERSIZED_PEAK],
        slab_double_frees: words[header::SLAB_DOUBLE_FREES],
        slab_accounting_errors: words[header::SLAB_ACCOUNTING_ERRORS],
        heap_total: words[header::HEAP_TOTAL],
        heap_used: words[header::HEAP_USED],
        heap_free: words[header::HEAP_FREE],
        cache_frames: words[header::CACHE_FRAMES],
        slab_frames: words[header::SLAB_FRAMES],
        tasks,
        raw: words[..WORDS].to_vec(),
    })
}

/// Decode a block from little-endian bytes (the Messenger reply form).
/// Returns `None` when the bytes are short or the version is not [`VERSION`].
pub fn decode_bytes(bytes: &[u8]) -> Option<Snapshot> {
    if bytes.len() < SIZE {
        return None;
    }
    let mut words = vec![0u64; WORDS];
    for (index, word) in words.iter_mut().enumerate() {
        let start = index * 8;
        *word = u64::from_le_bytes(bytes[start..start + 8].try_into().ok()?);
    }
    decode_words(&words)
}

/// Read one snapshot through the native syscall.
///
/// Checks the kernel-reported size against this client's [`SIZE`] first, so a
/// layout change is a clean `-EINVAL` instead of a decode of garbage. Returns
/// the negative errno on failure.
pub fn snapshot() -> Result<Snapshot, i64> {
    const EINVAL: i64 = crate::errno::EINVAL;
    if stats::system_stats_size()? != SIZE {
        return Err(-EINVAL);
    }
    let mut bytes = vec![0u8; SIZE];
    stats::system_stats_snapshot(&mut bytes)?;
    decode_bytes(&bytes).ok_or(-EINVAL)
}
