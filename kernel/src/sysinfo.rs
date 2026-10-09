//! System statistics snapshot (issue #144).
//!
//! `top`, `sysmond` and any dashboard need one read-only view of what the
//! kernel is doing: uptime, frame/slab/heap usage and the task table. This
//! module is that view, exposed as native syscall 14:
//!
//! ```text
//!   rax = 14  rdi = op
//!   op 0 (snapshot): rsi -> buffer, rdx = capacity in bytes -> SIZE | -errno
//!   op 1 (size):                                                -> SIZE
//! ```
//!
//! [`WORDS`] little-endian `u64`s are written in one fixed order: a
//! [`HEADER_WORDS`]-word header (version, sizes, uptime, memory counters) and
//! then one [`TASK_ROW_WORDS`]-word row per scheduler slot. [`SYSTEM_STATS_VERSION`]
//! is `4` (version 1 carried 16 rows, version 2 had 64 after issue #204,
//! version 3 has 256 for the application package system, version 4
//! appends the idle tick counter to the header so a monitor can tell idle
//! time from CPU time charged to tasks, and version 5 appends the frames the
//! block cache and the typed-object slabs hold, so a monitor can split memory
//! into programs, the kernel, the disk cache and free); a caller must reject a
//! header version it does not know. A buffer
//! smaller than [`SIZE`] is refused with `-E2BIG` (like the Messenger stats
//! op), a null buffer with `-EFAULT`, an unknown op with `-EINVAL`. The
//! `snapshot` op fills one stack array in place (never returned by value: at
//! 64 slots the block is over 5 KiB, and copies of it would eat the 32 KiB
//! kernel stack) under the subsystem locks taken one at a time, so it cannot
//! deadlock and cannot leak.
//!
//! # Permission
//!
//! Readable by every task, including unprivileged ones: this is the system
//! monitor, and the whole point is that a shell user can run `top`. The
//! snapshot is therefore limited to non-sensitive aggregate state — counters,
//! pids, states, classes, CPU ticks and task names. It deliberately excludes
//! page-table physical addresses, `fs_base`, heap breaks, credentials and
//! anything else that would leak an address or a secret; adding a field that
//! identifies *where* memory is or *who* a task is must be a new version with
//! an explicit capability check, not an extension of version 1.
//!
//! The wire form is mirrored (and decoded) by `libs/lazyos-sys/src/sysinfo/`,
//! and each field/index pair is duplicated as a compile-time-checked constant
//! there.

use alloc::vec::Vec;

use crate::mem;
use crate::task::{self, PriorityClass, TaskState, WaitKind};

/// ABI version of the block written by the `snapshot` op.
pub const SYSTEM_STATS_VERSION: u64 = 5;

/// Native system-stats ops (syscall 14).
pub mod op {
    /// Write the snapshot into the caller's buffer.
    pub const SNAPSHOT: u64 = 0;
    /// Report the snapshot's size in bytes, without touching a buffer.
    pub const SIZE: u64 = 1;
}

/// Task state codes in a row's state word. Stable across ABI versions.
pub mod state {
    /// Eligible for the scheduler.
    pub const RUNNABLE: u64 = 0;
    /// Parked on a wait queue; see the row's wait word.
    pub const BLOCKED: u64 = 1;
    /// Finished, waiting to be reaped or reclaimed.
    pub const DONE: u64 = 2;
}

/// Wait-kind codes in a blocked row's wait word (`0` = not blocked).
pub mod wait {
    pub const NONE: u64 = 0;
    pub const FUTEX: u64 = 1;
    pub const TERMINAL: u64 = 2;
    pub const CHILD_EXIT: u64 = 3;
    pub const SLEEP: u64 = 4;
    pub const PIPE: u64 = 5;
    pub const POLL: u64 = 6;
    pub const SIGNAL: u64 = 7;
    pub const SLOT: u64 = 8;
    pub const UNIX_ACCEPT: u64 = 9;
    pub const BLOCK: u64 = 10;
}

/// Scheduling-class codes. The order matches [`PriorityClass::ALL`].
pub mod class {
    pub const BACKGROUND: u64 = 0;
    pub const NORMAL: u64 = 1;
    pub const INTERACTIVE: u64 = 2;
    pub const REALTIME: u64 = 3;
}

/// Header word indices.
pub const H_VERSION: usize = 0;
/// Words in the whole block (header + task rows).
pub const H_WORDS: usize = 1;
/// PIT ticks since boot (100 Hz).
pub const H_TICKS: usize = 2;
/// Occupied slots whose state is not `done`.
pub const H_TASKS_LIVE: usize = 3;
/// Frames the allocator can hand out.
pub const H_FRAMES_TOTAL: usize = 4;
/// Frames currently handed out (`allocated - freed`).
pub const H_FRAMES_LIVE: usize = 5;
/// Frames on the free list.
pub const H_FRAMES_FREE: usize = 6;
/// Cumulative frame allocations.
pub const H_FRAMES_ALLOCATED: usize = 7;
/// Cumulative frame frees.
pub const H_FRAMES_FREED: usize = 8;
/// Frames held for the allocator's refcount table.
pub const H_FRAMES_RESERVED: usize = 9;
/// Double frees observed (should stay zero).
pub const H_FRAMES_DOUBLE_FREES: usize = 10;
/// Frees of non-usable addresses observed (should stay zero).
pub const H_FRAMES_INVALID_FREES: usize = 11;
/// Bytes live across all slab classes.
pub const H_SLAB_LIVE: usize = 12;
/// Slab live-bytes high-water mark.
pub const H_SLAB_PEAK: usize = 13;
/// Bytes live in the heap fallback for oversized slab requests.
pub const H_SLAB_OVERSIZED: usize = 14;
/// Oversized-bytes high-water mark.
pub const H_SLAB_OVERSIZED_PEAK: usize = 15;
/// Slab frees with no live slot (should stay zero).
pub const H_SLAB_DOUBLE_FREES: usize = 16;
/// Failed slab owner-accounting calls (should stay zero).
pub const H_SLAB_ACCOUNTING_ERRORS: usize = 17;
/// Bytes the kernel heap owns.
pub const H_HEAP_TOTAL: usize = 18;
/// Bytes currently handed out by the kernel heap.
pub const H_HEAP_USED: usize = 19;
/// Bytes on the kernel heap's free list.
pub const H_HEAP_FREE: usize = 20;
/// Words per task row ([`TASK_ROW_WORDS`], for a self-describing reader).
pub const H_TASK_ROW_WORDS: usize = 21;
/// Task rows following the header ([`task::MAX_TASKS`]).
pub const H_TASK_SLOTS: usize = 22;
/// PIT ticks (100 Hz) that found the CPU idle: no task was runnable. Busy
/// time is `H_TICKS - H_IDLE_TICKS`, so CPU load between two snapshots is
/// `1 - Δidle / Δticks`. Version 4.
pub const H_IDLE_TICKS: usize = 23;
/// Frames the ext2 block caches hold (file data kept for reuse, given back
/// under memory pressure). Part of `H_FRAMES_LIVE`. Version 5.
pub const H_CACHE_FRAMES: usize = 24;
/// Frames carved into typed-object slabs (`mem::slab`; never returned).
/// Part of `H_FRAMES_LIVE`, separate from the heap's `H_HEAP_TOTAL`. Version 5.
pub const H_SLAB_FRAMES: usize = 25;

/// Words in the header.
pub const HEADER_WORDS: usize = 26;
/// Words in one task row.
pub const TASK_ROW_WORDS: usize = 10;

/// Task row word indices (relative to the row's base).
pub const R_PRESENT: usize = 0;
/// Pid (the scheduler slot).
pub const R_PID: usize = 1;
/// Parent pid; `0` is the kernel/init task.
pub const R_PPID: usize = 2;
/// [`state`] code.
pub const R_STATE: usize = 3;
/// [`wait`] code when the state is `blocked`.
pub const R_WAIT: usize = 4;
/// [`class`] code.
pub const R_CLASS: usize = 5;
/// Weight inside the class.
pub const R_WEIGHT: usize = 6;
/// CPU ticks charged to the task.
pub const R_CPU_TICKS: usize = 7;
/// `fnv1a64` hash of the full task name.
pub const R_NAME_HASH: usize = 8;
/// Up to eight name bytes, packed little-endian and NUL-padded.
pub const R_NAME8: usize = 9;

/// Words in the whole block.
pub const WORDS: usize = HEADER_WORDS + task::MAX_TASKS * TASK_ROW_WORDS;
/// Bytes in the whole block.
pub const SIZE: u64 = (WORDS * 8) as u64;

/// Errno values, matching the Linux numbering the native ABI uses.
mod errno {
    pub const E2BIG: i64 = 7;
    pub const EFAULT: i64 = 14;
    pub const EINVAL: i64 = 22;
}

/// Two's-complement `-errno` in the syscall return register.
fn negative(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// The syscall entry point (syscall 14); returns `SIZE`, `0` or `-errno`.
pub fn dispatch(op: u64, a1: u64, a2: u64) -> u64 {
    match op {
        op::SIZE => SIZE,
        op::SNAPSHOT => snapshot(a1, a2),
        _ => negative(errno::EINVAL),
    }
}

/// op 0: copy the snapshot into the caller's buffer.
///
/// The buffer contract is strict: a caller must offer at least [`SIZE`] bytes,
/// so a partial or truncated snapshot can never be mistaken for a full one.
fn snapshot(buf: u64, capacity: u64) -> u64 {
    if buf == 0 {
        return negative(errno::EFAULT);
    }
    if capacity < SIZE {
        return negative(errno::E2BIG);
    }
    // Validate the whole `[buf, buf + SIZE)` range as mapped, writable user
    // memory before writing: a raw write would let any task aim the kernel at
    // a kernel address (CWE-787).
    // Heap, not stack: the block is ~20 KiB at 256 task rows, most of a kernel
    // stack. `snapshot_words` wants the fixed-size array view of the same Vec.
    let mut words: alloc::boxed::Box<[u64; WORDS]> = alloc::vec![0u64; WORDS]
        .into_boxed_slice()
        .try_into()
        .unwrap_or_else(|_| unreachable!("the Vec has exactly WORDS elements"));
    snapshot_words(&mut words);
    let mut bytes = Vec::with_capacity(SIZE as usize);
    for word in words.iter() {
        bytes.extend_from_slice(&word.to_ne_bytes());
    }
    match crate::ipc::syscalls::copy_out(buf, &bytes) {
        Ok(()) => SIZE,
        Err(code) => (code as u64).wrapping_neg(),
    }
}

/// Fill the fixed-layout snapshot into `words`. Takes each subsystem lock in
/// turn (frames, slab, heap, then the task table) and never two at once.
pub fn snapshot_words(words: &mut [u64; WORDS]) {
    let frames = mem::frame_stats();
    let slab = mem::slab::stats();
    let heap = mem::heap_stats();
    let tasks = task::stats_snapshot();

    words.fill(0);
    words[H_VERSION] = SYSTEM_STATS_VERSION;
    words[H_WORDS] = WORDS as u64;
    words[H_TICKS] = task::ticks();
    words[H_TASKS_LIVE] = tasks.live as u64;
    words[H_FRAMES_TOTAL] = frames.total as u64;
    words[H_FRAMES_LIVE] = frames.live() as u64;
    words[H_FRAMES_FREE] = frames.free as u64;
    words[H_FRAMES_ALLOCATED] = frames.allocated as u64;
    words[H_FRAMES_FREED] = frames.freed as u64;
    words[H_FRAMES_RESERVED] = frames.reserved as u64;
    words[H_FRAMES_DOUBLE_FREES] = frames.double_frees as u64;
    words[H_FRAMES_INVALID_FREES] = frames.invalid_frees as u64;
    words[H_SLAB_LIVE] = slab.live_bytes as u64;
    words[H_SLAB_PEAK] = slab.peak_bytes as u64;
    words[H_SLAB_OVERSIZED] = slab.oversized_bytes as u64;
    words[H_SLAB_OVERSIZED_PEAK] = slab.oversized_peak_bytes as u64;
    words[H_SLAB_DOUBLE_FREES] = slab.double_frees as u64;
    words[H_SLAB_ACCOUNTING_ERRORS] = slab.accounting_errors as u64;
    words[H_HEAP_TOTAL] = heap.total as u64;
    words[H_HEAP_USED] = heap.used as u64;
    words[H_HEAP_FREE] = heap.free as u64;
    words[H_TASK_ROW_WORDS] = TASK_ROW_WORDS as u64;
    words[H_TASK_SLOTS] = task::MAX_TASKS as u64;
    words[H_IDLE_TICKS] = task::idle_ticks();
    words[H_CACHE_FRAMES] = crate::fs::ext2::cache_frames() as u64;
    words[H_SLAB_FRAMES] = slab.classes.iter().map(|class| class.slabs as u64).sum();

    for (slot, row) in tasks.rows.iter().enumerate() {
        if !row.present {
            continue;
        }
        let base = HEADER_WORDS + slot * TASK_ROW_WORDS;
        words[base + R_PRESENT] = 1;
        words[base + R_PID] = row.pid as u64;
        words[base + R_PPID] = row.ppid as u64;
        words[base + R_STATE] = state_code(row.state);
        words[base + R_WAIT] = wait_code(row.state);
        words[base + R_CLASS] = class_code(row.class);
        words[base + R_WEIGHT] = row.weight as u64;
        words[base + R_CPU_TICKS] = row.cpu_ticks;
        words[base + R_NAME_HASH] = fnv1a64(row.name.as_bytes());
        words[base + R_NAME8] = u64::from_le_bytes(short_name(row.name));
    }
}

/// The [`state`] code of a task state.
fn state_code(state: TaskState) -> u64 {
    match state {
        TaskState::Runnable => state::RUNNABLE,
        TaskState::Blocked { .. } => state::BLOCKED,
        TaskState::Done => state::DONE,
    }
}

/// The [`wait`] code of a task state (`0` when not blocked).
fn wait_code(state: TaskState) -> u64 {
    match state {
        TaskState::Blocked { wait, .. } => match wait {
            WaitKind::Futex => wait::FUTEX,
            WaitKind::Terminal => wait::TERMINAL,
            WaitKind::ChildExit => wait::CHILD_EXIT,
            WaitKind::Sleep => wait::SLEEP,
            WaitKind::Pipe => wait::PIPE,
            WaitKind::Poll => wait::POLL,
            WaitKind::Signal => wait::SIGNAL,
            WaitKind::Slot => wait::SLOT,
            WaitKind::UnixAccept => wait::UNIX_ACCEPT,
            WaitKind::Block => wait::BLOCK,
        },
        _ => wait::NONE,
    }
}

/// The [`class`] code of a scheduling class.
fn class_code(class: PriorityClass) -> u64 {
    match class {
        PriorityClass::Background => class::BACKGROUND,
        PriorityClass::Normal => class::NORMAL,
        PriorityClass::Interactive => class::INTERACTIVE,
        PriorityClass::Realtime => class::REALTIME,
    }
}

/// Up to the first eight name bytes, NUL-padded. The first eight bytes are
/// enough for the short native service names (`kernel`, `sysmond`, `top`);
/// the name hash identifies anything longer.
pub fn short_name(name: &str) -> [u8; 8] {
    let mut short = [0u8; 8];
    let bytes = name.as_bytes();
    let count = bytes.len().min(short.len());
    short[..count].copy_from_slice(&bytes[..count]);
    short
}

/// `fnv1a64` of a task name, the stable identifier for a truncated name. The
/// same hash as the topic policy uses (`ipc::topics`), so tools share one.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
