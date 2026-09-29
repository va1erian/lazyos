//! The versioned `FabricStats` snapshot (syscall 5 `stats`) and the syscall-5
//! request that reads it.

use crate::sys::{self, msg_op, MsgArgs, MsgResult};

use super::error::{E2BIG, EINVAL};

/// Task slots in the per-slot arrays; mirrors `kernel::task::MAX_TASKS`.
pub const FABRIC_TASKS: usize = 64;

/// Bytes in the version-3 `FabricStats` block: 22 scalar words, 64 per-task
/// handle words, 8 ACL/audit words, and 64 four-word task rows.
pub const FABRIC_STATS_SIZE: usize = (22 + FABRIC_TASKS + 8 + FABRIC_TASKS * 4) * 8;

/// Per-slot usage row of a [`FabricStats`] snapshot.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct TaskUsage {
    /// `1` when a task occupies the slot.
    pub live: u64,
    /// Handles the task holds.
    pub handles: u64,
    /// Shared buffers the task created.
    pub buffers: u64,
    /// Buffer bytes charged to the task.
    pub buffer_bytes: u64,
}

/// The versioned fabric snapshot (stats ABI v3): channels, messages, buffers,
/// handles, fences, ACL/audit state, and per-slot usage in one block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FabricStats {
    /// ABI version; the kernel writes version 3 for a big-enough buffer.
    pub version: u64,
    /// Kernel-side services registered with the fabric.
    pub services: u64,
    /// Live channel endpoints (two per channel).
    pub endpoints: u64,
    /// Live channels.
    pub channels: u64,
    /// Messages currently queued.
    pub queued: u64,
    /// Parcel bytes currently queued.
    pub queued_bytes: u64,
    /// Transactions awaiting a reply.
    pub outstanding: u64,
    /// Synchronous calls started.
    pub calls: u64,
    /// Replies delivered.
    pub replies: u64,
    /// One-way messages accepted.
    pub one_way: u64,
    /// Transactions that hit their deadline.
    pub timeouts: u64,
    /// Transactions canceled by their caller.
    pub cancels: u64,
    /// Messages refused or discarded.
    pub drops: u64,
    /// Live shared buffers.
    pub buffers: u64,
    /// Bytes across live shared buffers.
    pub buffer_bytes: u64,
    /// Buffer mappings into task address spaces.
    pub buffer_mappings: u64,
    /// Cumulative fence submissions.
    pub fences_submitted: u64,
    /// Cumulative parked fence waits.
    pub fence_waits: u64,
    /// Fence waits that hit their deadline.
    pub fence_timeouts: u64,
    /// Submitted fence sequences not yet observed.
    pub outstanding_fences: u64,
    /// Zero-copy buffer handoffs.
    pub handoffs: u64,
    /// Handles held across every task.
    pub handles: u64,
    /// ACL rules installed.
    pub acl_rules: u64,
    /// `1` when a non-empty ACL policy is installed.
    pub acl_loaded: u64,
    /// `1` when allowed calls are audited.
    pub audit_trace: u64,
    /// Denials recorded since boot.
    pub audit_denies: u64,
    /// Allows recorded since boot (while tracing).
    pub audit_allows: u64,
    /// Events recorded since boot.
    pub audit_total: u64,
    /// Per-slot task usage.
    pub tasks: [TaskUsage; FABRIC_TASKS],
}

impl Default for FabricStats {
    fn default() -> Self {
        FabricStats {
            version: 0,
            services: 0,
            endpoints: 0,
            channels: 0,
            queued: 0,
            queued_bytes: 0,
            outstanding: 0,
            calls: 0,
            replies: 0,
            one_way: 0,
            timeouts: 0,
            cancels: 0,
            drops: 0,
            buffers: 0,
            buffer_bytes: 0,
            buffer_mappings: 0,
            fences_submitted: 0,
            fence_waits: 0,
            fence_timeouts: 0,
            outstanding_fences: 0,
            handoffs: 0,
            handles: 0,
            acl_rules: 0,
            acl_loaded: 0,
            audit_trace: 0,
            audit_denies: 0,
            audit_allows: 0,
            audit_total: 0,
            tasks: [TaskUsage::default(); FABRIC_TASKS],
        }
    }
}

impl FabricStats {
    /// The ABI version this mirror understands (3: 64 per-slot rows, #204).
    pub const VERSION: u64 = 3;

    /// Decode the little-endian word stream written by the `stats` op. `None`
    /// when the length is wrong or the version is newer than this mirror.
    pub fn from_bytes(bytes: &[u8]) -> Option<FabricStats> {
        if bytes.len() != FABRIC_STATS_SIZE {
            return None;
        }
        let word = |index: usize| -> Option<u64> {
            let at = index * 8;
            Some(u64::from_le_bytes(bytes[at..at + 8].try_into().ok()?))
        };
        let acl = 22 + FABRIC_TASKS;
        let mut tasks = [TaskUsage::default(); FABRIC_TASKS];
        for (index, usage) in tasks.iter_mut().enumerate() {
            let base = acl + 8 + index * 4;
            *usage = TaskUsage {
                live: word(base)?,
                handles: word(base + 1)?,
                buffers: word(base + 2)?,
                buffer_bytes: word(base + 3)?,
            };
        }
        let stats = FabricStats {
            version: word(0)?,
            services: word(1)?,
            endpoints: word(2)?,
            channels: word(3)?,
            queued: word(4)?,
            queued_bytes: word(5)?,
            outstanding: word(6)?,
            calls: word(7)?,
            replies: word(8)?,
            one_way: word(9)?,
            timeouts: word(10)?,
            cancels: word(11)?,
            drops: word(12)?,
            buffers: word(13)?,
            buffer_bytes: word(14)?,
            buffer_mappings: word(15)?,
            fences_submitted: word(16)?,
            fence_waits: word(17)?,
            fence_timeouts: word(18)?,
            outstanding_fences: word(19)?,
            handoffs: word(20)?,
            handles: word(21)?,
            acl_rules: word(acl)?,
            acl_loaded: word(acl + 1)?,
            audit_trace: word(acl + 2)?,
            audit_denies: word(acl + 3)?,
            audit_allows: word(acl + 4)?,
            audit_total: word(acl + 6)?,
            tasks,
        };
        if stats.version > Self::VERSION {
            return None;
        }
        Some(stats)
    }
}

/// Read the global fabric snapshot through syscall 5 `stats` (handle 0).
pub fn fabric_stats() -> Result<FabricStats, i64> {
    let mut buf = [0u8; FABRIC_STATS_SIZE];
    let args = MsgArgs {
        buf_ptr: buf.as_mut_ptr() as u64,
        buf_cap: buf.len() as u64,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    let code = sys::messenger(
        msg_op::STATS,
        &args as *const MsgArgs as u64,
        &mut result as *mut MsgResult as u64,
    );
    if code < 0 {
        return Err(code);
    }
    let len = result.bytes as usize;
    if len > buf.len() {
        return Err(-E2BIG);
    }
    FabricStats::from_bytes(&buf[..len]).ok_or(-EINVAL)
}
