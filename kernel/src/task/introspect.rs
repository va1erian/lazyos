//! Scheduler task-list introspection (MCP debug bridge, Phase 2).
//!
//! Mirrors the pattern [`crate::ipc::stats::FabricStats`] established for the
//! Messenger fabric: one versioned, fixed-layout snapshot of every task slot,
//! read under the same lock the scheduler already uses, with no new locking
//! model. Where `FabricStats` answers "what is the IPC fabric doing",
//! [`TaskSnapshot`] answers "what is the scheduler doing" — a live task list
//! with per-task state, priority class, weight and CPU time, which had no
//! query interface before this (see the "MCP Debug Bridge" design doc,
//! `docs/mcp-debug-bridge.md`).
//!
//! Exposed to userspace as native syscall 13 (`sys_tasks`, read-only, no
//! capability check: it discloses no more than `messengerctl sessions`
//! already does today). `messengerctl`'s `tasks-json` command prints it as a
//! single JSON line on serial, the same way `stats-json` does for
//! `FabricStats`, for `tools/mcp/debug_bridge.py` to pick up.

use alloc::vec::Vec;

use super::{PriorityClass, TaskState, MAX_TASKS, TASKS};

/// ABI version of the [`TaskSnapshot`] block.
pub const TASK_SNAPSHOT_VERSION: u64 = 3;

/// Words in one per-slot row: live, pid, ppid, pgid, sid, state tag, class,
/// weight, cpu_ticks, then two words (16 bytes) of the task name.
const ROW_WORDS: usize = 11;
const HEADER_WORDS: usize = 2;

/// Words in the whole [`TaskSnapshot`] block.
pub const WORDS: usize = HEADER_WORDS + MAX_TASKS * ROW_WORDS;

/// `TaskState` collapsed to a wire tag: `0` runnable, `1` blocked, `2` done.
fn state_tag(state: TaskState) -> u64 {
    match state {
        TaskState::Runnable => 0,
        TaskState::Blocked { .. } => 1,
        TaskState::Done => 2,
    }
}

/// `PriorityClass` collapsed to a wire tag, lowest first (matches
/// [`PriorityClass::ALL`]'s order).
fn class_tag(class: PriorityClass) -> u64 {
    match class {
        PriorityClass::Background => 0,
        PriorityClass::Normal => 1,
        PriorityClass::Interactive => 2,
        PriorityClass::Realtime => 3,
    }
}

/// A task name truncated to 16 bytes and packed into two little-endian words,
/// so the block stays fixed-size without a variable-length tail.
fn pack_name(name: &str) -> (u64, u64) {
    let bytes = name.as_bytes();
    let mut buf = [0u8; 16];
    let len = bytes.len().min(16);
    buf[..len].copy_from_slice(&bytes[..len]);
    (
        u64::from_le_bytes(buf[0..8].try_into().unwrap()),
        u64::from_le_bytes(buf[8..16].try_into().unwrap()),
    )
}

/// Unpack [`pack_name`]'s two words back into a display string (lossy: bytes
/// past the first invalid UTF-8 sequence, or past 16, are dropped).
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // exercised by TaskSnapshot::from_bytes tests
fn unpack_name(word0: u64, word1: u64) -> alloc::string::String {
    let mut buf = [0u8; 16];
    buf[0..8].copy_from_slice(&word0.to_le_bytes());
    buf[8..16].copy_from_slice(&word1.to_le_bytes());
    let end = buf.iter().position(|&b| b == 0).unwrap_or(16);
    alloc::string::String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// One task slot's row in a [`TaskSnapshot`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRow {
    pub live: bool,
    pub pid: u64,
    pub ppid: u64,
    pub pgid: u64,
    pub sid: u64,
    pub state: u64,
    pub class: u64,
    pub weight: u64,
    pub cpu_ticks: u64,
    pub name: alloc::string::String,
}

/// A full scheduler snapshot: one [`TaskRow`] per task slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskSnapshot {
    pub version: u64,
    pub rows: Vec<TaskRow>,
}

impl TaskSnapshot {
    pub const SIZE: usize = WORDS * 8;

    /// Take one snapshot of the task table. A single lock acquisition, so the
    /// view is fully consistent (unlike `FabricStats`, which samples several
    /// subsystems in turn).
    pub fn snapshot() -> TaskSnapshot {
        let tasks = TASKS.lock();
        let rows = tasks
            .iter()
            .map(|slot| match slot {
                Some(task) => TaskRow {
                    live: true,
                    pid: 0, // filled by the caller from the slot index; see `snapshot_words`.
                    ppid: task.parent as u64,
                    pgid: task.pgid as u64,
                    sid: task.sid as u64,
                    state: state_tag(task.state),
                    class: class_tag(task.class),
                    weight: task.weight as u64,
                    cpu_ticks: task.cpu_ticks,
                    name: alloc::string::String::from(task.name),
                },
                None => TaskRow {
                    live: false,
                    pid: 0,
                    ppid: 0,
                    pgid: 0,
                    sid: 0,
                    state: 0,
                    class: 0,
                    weight: 0,
                    cpu_ticks: 0,
                    name: alloc::string::String::new(),
                },
            })
            .enumerate()
            .map(|(slot, mut row)| {
                row.pid = slot as u64;
                row
            })
            .collect();
        TaskSnapshot {
            version: TASK_SNAPSHOT_VERSION,
            rows,
        }
    }

    /// Encode as little-endian words in field order (the syscall wire form).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut words: Vec<u64> = Vec::with_capacity(WORDS);
        words.push(self.version);
        words.push(self.rows.len() as u64);
        for row in &self.rows {
            let (name0, name1) = pack_name(&row.name);
            words.push(row.live as u64);
            words.push(row.pid);
            words.push(row.ppid);
            words.push(row.pgid);
            words.push(row.sid);
            words.push(row.state);
            words.push(row.class);
            words.push(row.weight);
            words.push(row.cpu_ticks);
            words.push(name0);
            words.push(name1);
        }
        // Pad to MAX_TASKS rows so the wire size is always `SIZE`, even if
        // fewer slots exist than `MAX_TASKS` (it never does today, but this
        // keeps the block fixed-size if that ever changes).
        while words.len() < WORDS {
            words.push(0);
        }
        debug_assert_eq!(words.len(), WORDS);
        let mut bytes = Vec::with_capacity(Self::SIZE);
        for word in words {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    /// Decode the block [`TaskSnapshot::to_bytes`] produces. `None` when the
    /// length is not exactly [`TaskSnapshot::SIZE`].
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // round-trip tested only
    pub fn from_bytes(bytes: &[u8]) -> Option<TaskSnapshot> {
        if bytes.len() != Self::SIZE {
            return None;
        }
        let word = |index: usize| -> Option<u64> {
            let at = index * 8;
            Some(u64::from_le_bytes(bytes[at..at + 8].try_into().ok()?))
        };
        let version = word(0)?;
        let count = word(1)? as usize;
        let mut rows = Vec::with_capacity(count.min(MAX_TASKS));
        for slot in 0..count.min(MAX_TASKS) {
            let base = HEADER_WORDS + slot * ROW_WORDS;
            rows.push(TaskRow {
                live: word(base)? != 0,
                pid: word(base + 1)?,
                ppid: word(base + 2)?,
                pgid: word(base + 3)?,
                sid: word(base + 4)?,
                state: word(base + 5)?,
                class: word(base + 6)?,
                weight: word(base + 7)?,
                cpu_ticks: word(base + 8)?,
                name: unpack_name(word(base + 9)?, word(base + 10)?),
            });
        }
        Some(TaskSnapshot { version, rows })
    }
}

/// Take one snapshot of the task table, encoded as the syscall's wire words.
#[allow(clippy::chunks_exact_to_as_chunks)] // as_chunks is unstable; chunks_exact is fine here
pub fn snapshot_words() -> Vec<u64> {
    let bytes = TaskSnapshot::snapshot().to_bytes();
    bytes
        .chunks_exact(8)
        .map(|chunk| u64::from_le_bytes(chunk.try_into().unwrap()))
        .collect()
}

const _: () = {
    assert!(WORDS == HEADER_WORDS + MAX_TASKS * ROW_WORDS);
};
