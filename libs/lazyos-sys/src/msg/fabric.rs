//! The versioned fabric snapshot (stats ABI v5) the `stats` op writes into a
//! buffer of [`FabricStats::SIZE`] bytes; mirrors `kernel/src/ipc/stats.rs`.

/// Task slots in the [`FabricStats`] per-slot arrays; mirrors the kernel's
/// `task::MAX_TASKS`.
pub const FABRIC_TASKS: usize = 256;

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

/// The versioned fabric snapshot (stats ABI version 5): channels, messages,
/// buffers, handles, ACL/audit state, and per-slot usage in one block. Mirrors
/// `kernel/src/ipc/stats.rs` field for field; [`FabricStats::from_bytes`]
/// decodes the little-endian word stream the kernel writes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FabricStats {
    /// ABI version; the kernel writes [`FabricStats::VERSION`].
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
    /// Zero-copy buffer handoffs.
    pub handoffs: u64,
    /// Handles held across every task.
    pub handles: u64,
    /// Handles held by each slot.
    pub handles_per_task: [u64; FABRIC_TASKS],
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
    /// Events retained in the audit ring.
    pub audit_count: u64,
    /// Events recorded since boot.
    pub audit_total: u64,
    /// Audit hash chain head.
    pub audit_last_hash: u64,
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
            handoffs: 0,
            handles: 0,
            handles_per_task: [0; FABRIC_TASKS],
            acl_rules: 0,
            acl_loaded: 0,
            audit_trace: 0,
            audit_denies: 0,
            audit_allows: 0,
            audit_count: 0,
            audit_total: 0,
            audit_last_hash: 0,
            tasks: [TaskUsage::default(); FABRIC_TASKS],
        }
    }
}

impl FabricStats {
    /// The ABI version this mirror understands (5: no fence counters, #677;
    /// 4: 256 per-slot rows; 3 had 64, #204).
    pub const VERSION: u64 = 5;
    /// Scalar words ahead of the per-slot handle counts.
    const SCALARS: usize = 18;
    /// Number of bytes the kernel writes for a snapshot.
    pub const SIZE: usize = (Self::SCALARS + FABRIC_TASKS + 8 + FABRIC_TASKS * 4) * 8;

    /// Decode the little-endian word stream written by the `stats` op. `None`
    /// when the length is not exactly [`FabricStats::SIZE`] or the version is
    /// newer than this mirror.
    pub fn from_bytes(bytes: &[u8]) -> Option<FabricStats> {
        if bytes.len() != Self::SIZE {
            return None;
        }
        let word = |index: usize| -> Option<u64> {
            let at = index * 8;
            Some(u64::from_le_bytes(bytes[at..at + 8].try_into().ok()?))
        };
        let mut handles_per_task = [0u64; FABRIC_TASKS];
        for (index, value) in handles_per_task.iter_mut().enumerate() {
            *value = word(Self::SCALARS + index)?;
        }
        let acl = Self::SCALARS + FABRIC_TASKS;
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
            handoffs: word(16)?,
            handles: word(17)?,
            handles_per_task,
            acl_rules: word(acl)?,
            acl_loaded: word(acl + 1)?,
            audit_trace: word(acl + 2)?,
            audit_denies: word(acl + 3)?,
            audit_allows: word(acl + 4)?,
            audit_count: word(acl + 5)?,
            audit_total: word(acl + 6)?,
            audit_last_hash: word(acl + 7)?,
            tasks,
        };
        if stats.version > Self::VERSION {
            return None;
        }
        Some(stats)
    }
}

/// Read the global fabric snapshot (`stats` with handle 0) into `buf`, which
/// must hold [`FabricStats::SIZE`] bytes; a polling loop reuses one buffer.
pub fn fabric_stats_into(buf: &mut [u8]) -> Result<FabricStats, i64> {
    let len = super::stats_bytes(0, buf)?;
    FabricStats::from_bytes(&buf[..len]).ok_or(-crate::errno::EINVAL)
}

/// [`fabric_stats_into`] with a buffer of its own.
#[cfg(feature = "alloc")]
pub fn fabric_stats() -> Result<FabricStats, i64> {
    fabric_stats_into(&mut alloc::vec![0u8; FabricStats::SIZE])
}
