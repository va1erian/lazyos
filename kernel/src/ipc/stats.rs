//! Fabric observability snapshot (issue #70).
//!
//! `docs/messenger.md` section 13 promises that the fabric is introspectable
//! from day one. This module is the kernel side of that promise: one
//! [`FabricStats`] snapshot aggregates the live state and cumulative counters of
//! every Messenger subsystem — channels and endpoints, message/transaction
//! counters, shared buffers, per-slot handle and buffer usage, the
//! ACL and audit rings, and the kernel services registered so far.
//!
//! [`FabricStats`] is also the versioned ABI block behind the native `stats`
//! syscall op. [`FABRIC_STATS_VERSION`] is 6 (version 2 had 16 per-slot rows,
//! version 3 had 64 after issue #204, version 4 has 256 for the application
//! package system, version 5 dropped the four fence counters, issue #677,
//! version 6 counts non-blocking polls apart from deadline expiries and adds
//! per-slot call counters, issue #702);
//! version 1 was the compact 64-byte
//! `MsgStats`. [`crate::ipc::syscalls`] serves v2 whenever the caller offers a
//! [`FabricStats::SIZE`]-byte buffer and keeps v1 for small buffers, so old
//! callers stay green. The wire form is little-endian `u64` words in field
//! order; `user/src/messenger/` mirrors the decode byte for byte.
//!
//! The snapshot is a pure read: it takes each subsystem's lock in turn and never
//! holds two at once, so collecting stats can never block a message in flight
//! for longer than one counter read.

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::{acl, audit, channels, handles, shared};
use crate::task::MAX_TASKS;

/// ABI version of the [`FabricStats`] block written by the native `stats` op.
///
/// * `1` — the compact 64-byte [`crate::ipc::syscalls::MsgStats`] counters.
/// * `2` — this snapshot: every subsystem, per-slot usage included.
/// * `5` — no fence counters (issue #677).
/// * `6` — `polls`, and per-slot `calls`/`timeouts`/`polls` (issue #702).
pub const FABRIC_STATS_VERSION: u64 = 6;

/// Words in one per-slot task usage row (see [`TaskUsage`]).
const TASK_USAGE_WORDS: usize = 7;

/// Words in the snapshot: scalars, the handle table, the ACL/audit block, then
/// the per-slot rows. Used for [`FabricStats::SIZE`] and the field-order decode.
const SCALAR_WORDS: usize = 19;
const ACL_AUDIT_WORDS: usize = 8;

/// Words in the [`FabricStats`] block.
pub const WORDS: usize = SCALAR_WORDS + MAX_TASKS + ACL_AUDIT_WORDS + MAX_TASKS * TASK_USAGE_WORDS;

/// Per-slot fabric usage: whether a task occupies the slot and how much of the
/// fabric it holds. Indexed by task slot (`0` is the kernel task).
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct TaskUsage {
    /// `1` when a task occupies this slot.
    pub live: u64,
    /// Handles the task currently holds.
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

/// One snapshot of the whole Messenger fabric.
///
/// Every field is a `u64` so the ABI block is trivially little-endian; the
/// arrays are fixed to the kernel task count. `admin` counters
/// ([`FabricStats::audit_denies`], [`FabricStats::audit_allows`],
/// [`FabricStats::audit_total`]) are cumulative since boot, while
/// [`FabricStats::queued`], [`FabricStats::outstanding`] and the buffer totals
/// are live depths.
#[repr(C)]
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FabricStats {
    /// [`FABRIC_STATS_VERSION`]: lets a caller detect a newer ABI.
    pub version: u64,
    /// Kernel-side services registered with the fabric (the bootstrap
    /// `messengerd` stub is the only one today).
    pub services: u64,
    /// Live channel endpoints across every channel (two per channel).
    pub endpoints: u64,
    /// Live channels in the kernel registry.
    pub channels: u64,
    /// Messages currently queued on every endpoint (queue depth).
    pub queued: u64,
    /// Parcel bytes currently queued.
    pub queued_bytes: u64,
    /// Transactions currently awaiting a reply.
    pub outstanding: u64,
    /// Synchronous calls started.
    pub calls: u64,
    /// Replies delivered.
    pub replies: u64,
    /// One-way messages accepted.
    pub one_way: u64,
    /// Transactions that hit a real deadline: nobody answered in time.
    pub timeouts: u64,
    /// Non-blocking polls that ended unanswered. Every call ends in a reply,
    /// a timeout, a poll, a cancel or a dead peer; empty polls are routine
    /// background traffic, not a health signal (issue #702).
    pub polls: u64,
    /// Transactions canceled by their caller.
    pub cancels: u64,
    /// Messages refused or discarded.
    pub drops: u64,
    /// Live shared buffers.
    pub buffers: u64,
    /// Bytes across live shared buffers.
    pub buffer_bytes: u64,
    /// Mappings of shared buffers into task address spaces.
    pub buffer_mappings: u64,
    /// Buffer descriptors delivered to a receiver without copying.
    pub handoffs: u64,
    /// Handles held across every task.
    pub handles: u64,
    /// Handles held by each slot (index = task slot).
    pub handles_per_task: [u64; MAX_TASKS],
    /// Rules in the installed ACL policy.
    pub acl_rules: u64,
    /// `1` when a non-empty ACL policy is installed.
    pub acl_loaded: u64,
    /// `1` when allowed calls are also being audited.
    pub audit_trace: u64,
    /// Denials recorded since boot.
    pub audit_denies: u64,
    /// Allows recorded since boot (present only while tracing).
    pub audit_allows: u64,
    /// Events currently retained in the audit ring (bounded, may have wrapped).
    pub audit_count: u64,
    /// Events recorded since boot (monotonic).
    pub audit_total: u64,
    /// The audit hash chain head; the next record chains from here.
    pub audit_last_hash: u64,
    /// Per-slot task usage (index = task slot).
    pub tasks: [TaskUsage; MAX_TASKS],
}

impl Default for FabricStats {
    fn default() -> Self {
        FabricStats {
            version: FABRIC_STATS_VERSION,
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
            polls: 0,
            cancels: 0,
            drops: 0,
            buffers: 0,
            buffer_bytes: 0,
            buffer_mappings: 0,
            handoffs: 0,
            handles: 0,
            handles_per_task: [0; MAX_TASKS],
            acl_rules: 0,
            acl_loaded: 0,
            audit_trace: 0,
            audit_denies: 0,
            audit_allows: 0,
            audit_count: 0,
            audit_total: 0,
            audit_last_hash: audit::GENESIS_HASH,
            tasks: [TaskUsage::default(); MAX_TASKS],
        }
    }
}

impl FabricStats {
    /// Number of bytes the snapshot occupies on the wire.
    pub const SIZE: usize = WORDS * 8;

    /// Take one consistent-enough read of every subsystem. Counters are sampled
    /// subsystem by subsystem; a message that lands between two reads may be
    /// counted in one and not the other, which is exactly what "snapshot" means.
    ///
    /// Boxed and filled in place: at 256 slots the block is ~10 KiB, and a
    /// by-value return would copy it through a 32 KiB kernel stack twice.
    pub fn snapshot() -> Box<FabricStats> {
        let channel_stats = channels::stats();
        let channel_counts = channels::counts();
        let buffer_stats = shared::stats();
        let slot_totals = channels::slot_totals();

        // SAFETY: `FabricStats` is `repr(C)` and made only of `u64`s and arrays
        // of `u64`-only structs, so the all-zero pattern is a valid value.
        let mut stats: Box<FabricStats> = unsafe { Box::new_zeroed().assume_init() };

        // Handles per slot, and their total, from the per-task tables.
        let mut handles_total = 0u64;
        for (slot, count) in stats.handles_per_task.iter_mut().enumerate() {
            *count = handles::count_for_task(slot) as u64;
            handles_total += *count;
        }

        // Per-slot usage joins the task table (is anyone there?) with the
        // handle and buffer accounting read above.
        let processes = crate::task::process::process_list();
        for slot in 0..MAX_TASKS {
            let buffer = shared::process_stats(slot);
            let totals = slot_totals.get(slot).copied().unwrap_or_default();
            stats.tasks[slot] = TaskUsage {
                live: processes.iter().any(|process| process.slot == slot) as u64,
                handles: stats.handles_per_task[slot],
                buffers: buffer.buffers,
                buffer_bytes: buffer.bytes,
                calls: totals.calls,
                timeouts: totals.timeouts,
                polls: totals.polls,
            };
        }

        // Scalars last: assigned field by field so the arrays above are never
        // copied through the stack.
        stats.version = FABRIC_STATS_VERSION;
        stats.services = super::syscalls::bootstrap::service_handle().is_some() as u64;
        stats.endpoints = channel_counts.endpoints;
        stats.channels = channel_counts.channels;
        stats.queued = channel_stats.queued;
        stats.queued_bytes = channel_stats.queued_bytes;
        stats.outstanding = channel_stats.outstanding;
        stats.calls = channel_stats.calls;
        stats.replies = channel_stats.replies;
        stats.one_way = channel_stats.one_way;
        stats.timeouts = channel_stats.timeouts;
        stats.polls = channel_stats.polls;
        stats.cancels = channel_stats.cancels;
        stats.drops = channel_stats.drops;
        stats.buffers = buffer_stats.buffers;
        stats.buffer_bytes = buffer_stats.bytes;
        stats.buffer_mappings = buffer_stats.mappings;
        stats.handoffs = buffer_stats.handoffs;
        stats.handles = handles_total;
        stats.acl_rules = acl::rule_count() as u64;
        stats.acl_loaded = acl::is_loaded() as u64;
        stats.audit_trace = audit::trace() as u64;
        stats.audit_denies = audit::denials();
        stats.audit_allows = audit::allows();
        stats.audit_count = audit::count() as u64;
        stats.audit_total = audit::total();
        stats.audit_last_hash = audit::last_hash();
        stats
    }

    /// Encode as little-endian words in field order (the syscall wire form).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut words: Vec<u64> = Vec::with_capacity(WORDS);
        words.push(self.version);
        words.push(self.services);
        words.push(self.endpoints);
        words.push(self.channels);
        words.push(self.queued);
        words.push(self.queued_bytes);
        words.push(self.outstanding);
        words.push(self.calls);
        words.push(self.replies);
        words.push(self.one_way);
        words.push(self.timeouts);
        words.push(self.polls);
        words.push(self.cancels);
        words.push(self.drops);
        words.push(self.buffers);
        words.push(self.buffer_bytes);
        words.push(self.buffer_mappings);
        words.push(self.handoffs);
        words.push(self.handles);
        words.extend_from_slice(&self.handles_per_task);
        words.push(self.acl_rules);
        words.push(self.acl_loaded);
        words.push(self.audit_trace);
        words.push(self.audit_denies);
        words.push(self.audit_allows);
        words.push(self.audit_count);
        words.push(self.audit_total);
        words.push(self.audit_last_hash);
        for task in &self.tasks {
            words.push(task.live);
            words.push(task.handles);
            words.push(task.buffers);
            words.push(task.buffer_bytes);
            words.push(task.calls);
            words.push(task.timeouts);
            words.push(task.polls);
        }
        debug_assert_eq!(words.len(), WORDS);
        let mut bytes = Vec::with_capacity(Self::SIZE);
        for word in words {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    /// Decode the block [`FabricStats::to_bytes`] produces. `None` when the
    /// length is not exactly [`FabricStats::SIZE`].
    pub fn from_bytes(bytes: &[u8]) -> Option<Box<FabricStats>> {
        if bytes.len() != Self::SIZE {
            return None;
        }
        let word = |index: usize| -> Option<u64> {
            let at = index * 8;
            Some(u64::from_le_bytes(bytes[at..at + 8].try_into().ok()?))
        };
        // SAFETY: see `snapshot`: all-zero is a valid `FabricStats`.
        let mut stats: Box<FabricStats> = unsafe { Box::new_zeroed().assume_init() };
        for (index, value) in stats.handles_per_task.iter_mut().enumerate() {
            *value = word(SCALAR_WORDS + index)?;
        }
        let acl = SCALAR_WORDS + MAX_TASKS;
        for (index, usage) in stats.tasks.iter_mut().enumerate() {
            let base = acl + ACL_AUDIT_WORDS + index * TASK_USAGE_WORDS;
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

/// Take one snapshot of the whole fabric.
pub fn snapshot() -> Box<FabricStats> {
    FabricStats::snapshot()
}

/// Reset every fabric subsystem and its counters. `Test-harness only`: a
/// running system must never lose its handles, buffers, or audit trail.
#[cfg(lazyos_tests)]
pub fn reset() {
    use super::{credentials, syscalls};
    channels::reset();
    shared::reset();
    syscalls::bootstrap::reset();
    for slot in 0..MAX_TASKS {
        handles::reset_for_task(slot);
        credentials::reset_for_task(slot);
    }
    acl::load(&[]);
    audit::reset();
    audit::set_trace(false);
}

const _: () = {
    // The ABI layout is words in field order; an accidental field addition is a
    // compile error, not a silent wire-format change.
    assert!(core::mem::size_of::<TaskUsage>() == TASK_USAGE_WORDS * 8);
    assert!(core::mem::size_of::<FabricStats>() == FabricStats::SIZE);
};
