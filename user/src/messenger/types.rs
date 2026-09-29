//! Wire ABI blocks, stats snapshots, and the [`Error`] type shared by every
//! Messenger client and service protocol.
//!
//! The ABI blocks below mirror `kernel/src/ipc/syscalls.rs` field for field;
//! keep them in lockstep (the kernel pins the sizes at compile time).

use super::errno;
use libmessenger::Error as ParcelError;

/// The syscall request block; mirrors the kernel's `MsgArgs`.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MsgArgs {
    /// Endpoint handle: call, begin, send, recv, cancel, close, stats.
    pub handle: u64,
    /// Transaction id: reply, cancel, await.
    pub txn_id: u64,
    /// Request parcel bytes (call, begin, send, reply).
    pub parcel_ptr: u64,
    /// Request parcel length in bytes.
    pub parcel_len: u64,
    /// Reply or receive buffer (call, recv, await, stats).
    pub buf_ptr: u64,
    /// Capacity of `buf_ptr` in bytes.
    pub buf_cap: u64,
    /// Absolute PIT deadline; 0 waits forever.
    pub deadline: u64,
    /// Reserved; must be zero.
    pub flags: u64,
}

/// The syscall response block; mirrors the kernel's `MsgResult`.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MsgResult {
    /// 0 on success, or a negative errno.
    pub status: i64,
    /// New handle (create_pair/bootstrap), transaction id (begin/recv).
    pub value: u64,
    /// Second handle (create_pair), sender task slot (recv).
    pub aux: u64,
    /// Bytes written to `buf_ptr` (call, await, recv, stats).
    pub bytes: u64,
    /// `recv` reports the delivered transfers here: `[first handle, handle
    /// count, first buffer handle, buffer count]`; zero otherwise.
    pub reserved: [u64; 4],
}

/// Channel counters; mirrors the kernel's `MsgStats` byte for byte.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Stats {
    pub calls: u64,
    pub replies: u64,
    pub timeouts: u64,
    pub cancels: u64,
    pub drops: u64,
    pub queued: u64,
    pub queued_bytes: u64,
    pub outstanding: u64,
}

impl Stats {
    /// Number of bytes the compact `stats` shape occupies.
    pub const SIZE: usize = 64;
}

/// Task slots in the [`FabricStats`] per-slot arrays; mirrors the kernel's
/// `task::MAX_TASKS`.
pub const FABRIC_TASKS: usize = 64;

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

/// The versioned fabric snapshot (stats ABI version 3): channels, messages,
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
            fences_submitted: 0,
            fence_waits: 0,
            fence_timeouts: 0,
            outstanding_fences: 0,
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
    /// The ABI version this mirror understands (3: 64 per-slot rows, #204).
    pub const VERSION: u64 = 3;
    /// Number of bytes the kernel writes for a snapshot.
    pub const SIZE: usize = (22 + FABRIC_TASKS + 8 + FABRIC_TASKS * 4) * 8;

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
            *value = word(22 + index)?;
        }
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

/// A Messenger or kernel error.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The kernel refused the operation with a negative errno.
    Errno(i64),
    /// The registry daemon refused the request with a positive errno-style
    /// code (`registry::serve_request` carries it across the channel).
    Registry(i64),
    /// The topics broker refused the request with a positive errno-style code
    /// (`topics` carries it in the reply's `ERROR` field).
    Topics(i64),
    /// The MIME service refused the request with a positive errno-style code
    /// (`mimed` carries it in the reply's `ERROR` field).
    Mime(i64),
    /// The supervisor refused the request with a positive errno-style code
    /// (`init` carries it in the reply's `ERROR` field).
    Init(i64),
    /// The configuration registry refused the request with a `REGD_*` code
    /// (`regd` carries it in the reply's `ERROR` field).
    Regd(i64),
    /// A parcel was malformed on encode or decode.
    Parcel(ParcelError),
}

impl Error {
    /// The negative errno the kernel returned, if this is a kernel error.
    pub fn errno(self) -> Option<i64> {
        match self {
            Error::Errno(code) => Some(code),
            // Registry and broker codes travel positive; normalise to the
            // syscall shape.
            Error::Registry(code) | Error::Topics(code) | Error::Mime(code) | Error::Init(code) => {
                Some(-code)
            }
            Error::Regd(code) => Some(-code),
            Error::Parcel(_) => None,
        }
    }

    /// A short, human-readable explanation (friendly-errors convention).
    pub fn message(self) -> &'static str {
        match self {
            Error::Parcel(error) => error.message(),
            Error::Registry(code) => registry_message(code),
            Error::Topics(code) => topics_message(code),
            Error::Mime(code) => mime_message(code),
            Error::Init(code) => init_message(code),
            Error::Regd(code) => regd_message(code),
            // A match guard keeps the named constants readable; a bare
            // `-CONST` is not a valid pattern.
            Error::Errno(code) => match code {
                code if code == -errno::EPERM => {
                    "this app is not allowed to perform that Messenger operation"
                }
                code if code == -errno::ENOENT => "no service is registered under that name",
                code if code == -errno::E2BIG => {
                    "the parcel or reply exceeds the Messenger buffer limit"
                }
                code if code == -errno::EAGAIN => "the peer's queue is full; try again shortly",
                code if code == -errno::ENOMEM => "the kernel is out of Messenger resources",
                code if code == -errno::EACCES => {
                    "this app is not allowed to make that Messenger call"
                }
                code if code == -errno::EFAULT => "a Messenger buffer pointer is invalid",
                code if code == -errno::EBUSY => "the bootstrap endpoint has already been claimed",
                code if code == -errno::EEXIST => "that service name is already registered",
                code if code == -errno::EINVAL => "the Messenger request is malformed",
                code if code == -errno::EPIPE => "the other end of the channel closed",
                code if code == -errno::EDEADLK => {
                    "the call would deadlock with an open transaction"
                }
                code if code == -errno::EBADMSG => {
                    "the wrapped value failed authentication or is malformed"
                }
                code if code == -errno::ETIMEDOUT => "the call timed out before a reply arrived",
                code if code == -errno::ECANCELED => "the call was canceled",
                _ => "the Messenger call failed",
            },
        }
    }
}

/// Friendly text for a registry error code crossing the daemon protocol. The
/// strings mirror `kernel/src/ipc/registry.rs` so both paths explain a failure
/// the same way.
fn registry_message(code: i64) -> &'static str {
    if code == errno::EPERM {
        "only the owner (or an administrator) may unregister that name"
    } else if code == errno::ENOENT {
        "no service is registered under that name"
    } else if code == errno::EEXIST {
        "that service name is already registered by another owner"
    } else if code == errno::ENOMEM {
        "the kernel name registry is full"
    } else if code == errno::EACCES {
        "this app is not allowed to use the name registry"
    } else {
        "the registry request is malformed"
    }
}

/// Friendly text for a topics-broker error code crossing the daemon protocol.
fn topics_message(code: i64) -> &'static str {
    if code == errno::EACCES {
        "this app is not allowed to use that topic segment"
    } else if code == errno::ENOENT {
        "no such topic subscription"
    } else if code == errno::EINVAL {
        "the topic name or filter is malformed"
    } else if code == errno::EPERM {
        "that subscription belongs to another task"
    } else if code == errno::E2BIG {
        "the event payload exceeds the topic broker's limit"
    } else if code == errno::ENOMEM {
        "the topic broker is out of subscription slots"
    } else {
        "the topic request failed"
    }
}

/// Friendly text for a MIME-service error code crossing the daemon protocol.
fn mime_message(code: i64) -> &'static str {
    if code == errno::ENOENT {
        "no application is registered for that file type and verb"
    } else if code == errno::EINVAL {
        "the MIME request is malformed"
    } else if code == errno::E2BIG {
        "the MIME reply exceeds the Messenger buffer limit"
    } else {
        "the MIME request failed"
    }
}

/// Friendly text for a supervisor (`init`) error code crossing the protocol.
fn init_message(code: i64) -> &'static str {
    if code == errno::EPERM {
        "this task may not launch into that session"
    } else if code == errno::ENOENT {
        "no such app, session, or program"
    } else if code == errno::EINVAL {
        "the launch request is malformed"
    } else if code == errno::ENOMEM {
        "the supervisor has no free task slot"
    } else if code == errno::EAGAIN {
        "this session already has too many launched apps running; try again once one exits"
    } else {
        "the supervisor request failed"
    }
}

/// Friendly text for a `regd` error code crossing the protocol. The constants
/// live with the wire module so the service and client agree on one set.
fn regd_message(code: i64) -> &'static str {
    use super::regd;
    if code == regd::REGD_NOT_FOUND {
        "no value is stored at that path"
    } else if code == regd::REGD_BAD_PATH {
        "that is not a valid regd path"
    } else if code == regd::REGD_TOO_LARGE {
        "the value or store exceeds a regd size limit"
    } else if code == regd::REGD_DENIED {
        "the caller may not access that path"
    } else if code == regd::REGD_BAD_VALUE {
        "the request carried a malformed regd value"
    } else if code == regd::REGD_IO {
        "the regd store could not be read or written"
    } else {
        "the regd request failed"
    }
}

/// Result alias for the userspace API.
pub type Result<T> = core::result::Result<T, Error>;

/// Default reply/receive buffer for the convenience methods. A reply that does
/// not fit is refused with `-E2BIG` *after* the transaction completes, so the
/// bytes are lost; a streaming/shared-buffer path is the follow-up for large
/// payloads (`docs/messenger.md` section 10).
pub const DEFAULT_BUFFER: usize = 16 * 1024;

/// Absolute PIT tick used to ask for an immediate `recv` answer (issue #91).
///
/// The native surface has no "peek" op, but a deadline at or below the current
/// tick is already expired when `recv` parks: the timer gate sweeps it on the
/// spot and reports `-ETIMEDOUT` instead of blocking for a message. Tick 1 is
/// in the past after the first 10 ms of boot, so [`Endpoint::poll_recv`] is
/// immediate from then on; before the first tick it waits at most one tick.
pub const EXPIRED_DEADLINE: u64 = 1;
