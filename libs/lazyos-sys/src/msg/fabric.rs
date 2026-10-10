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
    /// Synchronous calls the task started (live channels only).
    pub calls: u64,
    /// Its calls that missed a real deadline.
    pub timeouts: u64,
    /// Its non-blocking polls that ended unanswered.
    pub polls: u64,
}

/// The versioned fabric snapshot (stats ABI version 6): channels, messages,
/// buffers, handles, ACL/audit state, and per-slot usage in one block. Mirrors
/// `kernel/src/ipc/stats.rs` field for field; [`FabricStats::from_bytes`]
/// decodes the little-endian word stream the kernel writes.
///
/// ~16 KiB, so it is only ever built in place on the heap (`Box`): a native
/// program's stack is small, and a by-value snapshot overflowed it (#702).
#[derive(Clone, PartialEq, Eq, Debug)]
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
    /// Transactions that hit a real deadline.
    pub timeouts: u64,
    /// Non-blocking polls that ended unanswered (background traffic, not
    /// failures; issue #702).
    pub polls: u64,
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

impl FabricStats {
    /// The ABI version this mirror understands (6: `polls` and per-slot call
    /// counters, #702; 5: no fence counters, #677; 4: 256 per-slot rows; 3
    /// had 64, #204).
    pub const VERSION: u64 = 6;
    /// Scalar words ahead of the per-slot handle counts.
    const SCALARS: usize = 19;
    /// Words in one per-slot usage row.
    const USAGE_WORDS: usize = 7;
    /// Number of bytes the kernel writes for a snapshot.
    pub const SIZE: usize =
        (Self::SCALARS + FABRIC_TASKS + 8 + FABRIC_TASKS * Self::USAGE_WORDS) * 8;

    /// Decode the little-endian word stream written by the `stats` op, in
    /// place on the heap. `None` when the length is not exactly
    /// [`FabricStats::SIZE`] or the version is newer than this mirror.
    #[cfg(feature = "alloc")]
    pub fn from_bytes(bytes: &[u8]) -> Option<alloc::boxed::Box<FabricStats>> {
        if bytes.len() != Self::SIZE {
            return None;
        }
        let word = |index: usize| -> Option<u64> {
            let at = index * 8;
            Some(u64::from_le_bytes(bytes[at..at + 8].try_into().ok()?))
        };
        // SAFETY: `FabricStats` is made only of `u64`s and arrays of
        // `u64`-only structs, so the all-zero pattern is a valid value.
        let mut stats: alloc::boxed::Box<FabricStats> =
            unsafe { alloc::boxed::Box::new_zeroed().assume_init() };
        for (index, value) in stats.handles_per_task.iter_mut().enumerate() {
            *value = word(Self::SCALARS + index)?;
        }
        let acl = Self::SCALARS + FABRIC_TASKS;
        for (index, usage) in stats.tasks.iter_mut().enumerate() {
            let base = acl + 8 + index * Self::USAGE_WORDS;
            *usage = TaskUsage {
                live: word(base)?,
                handles: word(base + 1)?,
                buffers: word(base + 2)?,
                buffer_bytes: word(base + 3)?,
                calls: word(base + 4)?,
                timeouts: word(base + 5)?,
                polls: word(base + 6)?,
            };
        }
        stats.version = word(0)?;
        if stats.version > Self::VERSION {
            return None;
        }
        stats.services = word(1)?;
        stats.endpoints = word(2)?;
        stats.channels = word(3)?;
        stats.queued = word(4)?;
        stats.queued_bytes = word(5)?;
        stats.outstanding = word(6)?;
        stats.calls = word(7)?;
        stats.replies = word(8)?;
        stats.one_way = word(9)?;
        stats.timeouts = word(10)?;
        stats.polls = word(11)?;
        stats.cancels = word(12)?;
        stats.drops = word(13)?;
        stats.buffers = word(14)?;
        stats.buffer_bytes = word(15)?;
        stats.buffer_mappings = word(16)?;
        stats.handoffs = word(17)?;
        stats.handles = word(18)?;
        stats.acl_rules = word(acl)?;
        stats.acl_loaded = word(acl + 1)?;
        stats.audit_trace = word(acl + 2)?;
        stats.audit_denies = word(acl + 3)?;
        stats.audit_allows = word(acl + 4)?;
        stats.audit_count = word(acl + 5)?;
        stats.audit_total = word(acl + 6)?;
        stats.audit_last_hash = word(acl + 7)?;
        Some(stats)
    }
}

/// Read the global fabric snapshot (`stats` with handle 0) into `buf`, which
/// must hold [`FabricStats::SIZE`] bytes; a polling loop reuses one buffer.
#[cfg(feature = "alloc")]
pub fn fabric_stats_into(buf: &mut [u8]) -> Result<alloc::boxed::Box<FabricStats>, i64> {
    let len = super::stats_bytes(0, buf)?;
    FabricStats::from_bytes(&buf[..len]).ok_or(-crate::errno::EINVAL)
}

/// [`fabric_stats_into`] with a buffer of its own.
#[cfg(feature = "alloc")]
pub fn fabric_stats() -> Result<alloc::boxed::Box<FabricStats>, i64> {
    fabric_stats_into(&mut alloc::vec![0u8; FabricStats::SIZE])
}
