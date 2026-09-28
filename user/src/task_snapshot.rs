//! Scheduler task-list introspection (MCP debug bridge Phase 2).
//!
//! Mirrors `messenger::FabricStats` for scheduler state instead of the IPC
//! fabric: [`TaskSnapshot::from_bytes`] decodes the little-endian word stream
//! `kernel/src/task/introspect.rs` writes via native syscall 13
//! (`sys_tasks`). `messengerctl`'s `tasks-json` command uses this to print a
//! machine-parseable line for `tools/mcp/debug_bridge.py`.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::messenger::{Error, Result};
use crate::sys;

/// Task slots in a [`TaskSnapshot`]; mirrors the kernel's `task::MAX_TASKS`.
pub const MAX_TASKS: usize = 64;

/// One task slot's row.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TaskRow {
    pub live: bool,
    pub pid: u64,
    pub ppid: u64,
    pub pgid: u64,
    pub sid: u64,
    /// `0` runnable, `1` blocked, `2` done.
    pub state: u64,
    /// `0` background, `1` normal, `2` interactive, `3` realtime.
    pub class: u64,
    pub weight: u64,
    pub cpu_ticks: u64,
    pub name: String,
}

/// A full scheduler snapshot: one [`TaskRow`] per live task slot.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TaskSnapshot {
    pub version: u64,
    pub rows: Vec<TaskRow>,
}

impl TaskSnapshot {
    const HEADER_WORDS: usize = 2;
    const ROW_WORDS: usize = 11;
    const WORDS: usize = Self::HEADER_WORDS + MAX_TASKS * Self::ROW_WORDS;

    /// Number of bytes the snapshot occupies on the wire.
    pub const SIZE: usize = Self::WORDS * 8;

    /// Decode the block the kernel's `sys_tasks` writes. `None` when the
    /// length is not exactly [`TaskSnapshot::SIZE`].
    pub fn from_bytes(bytes: &[u8]) -> Option<TaskSnapshot> {
        if bytes.len() != Self::SIZE {
            return None;
        }
        let word = |index: usize| -> Option<u64> {
            let at = index * 8;
            Some(u64::from_le_bytes(bytes[at..at + 8].try_into().ok()?))
        };
        let version = word(0)?;
        let count = (word(1)? as usize).min(MAX_TASKS);
        let mut rows = Vec::with_capacity(count);
        for slot in 0..count {
            let base = Self::HEADER_WORDS + slot * Self::ROW_WORDS;
            let name0 = word(base + 9)?;
            let name1 = word(base + 10)?;
            let mut buf = [0u8; 16];
            buf[0..8].copy_from_slice(&name0.to_le_bytes());
            buf[8..16].copy_from_slice(&name1.to_le_bytes());
            let end = buf.iter().position(|&b| b == 0).unwrap_or(16);
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
                name: String::from_utf8_lossy(&buf[..end]).into_owned(),
            });
        }
        Some(TaskSnapshot { version, rows })
    }
}

/// Read the live scheduler snapshot from the kernel.
pub fn task_snapshot() -> Result<TaskSnapshot> {
    let mut buf = vec![0u8; TaskSnapshot::SIZE];
    let code = sys::tasks(buf.as_mut_ptr() as u64);
    if code != 0 {
        return Err(Error::Errno(code));
    }
    TaskSnapshot::from_bytes(&buf).ok_or(Error::Errno(-22 /* EINVAL */))
}
