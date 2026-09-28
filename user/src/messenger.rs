//! Blocking userspace Messenger API over the native `messenger` syscall
//! (issue #69).
//!
//! This is the synchronous `Connection::call` / `Server::serve` shape from
//! `docs/messenger.md` section 15, layered straight on [`crate::sys`]: each
//! operation is one `int 0x80` with a small request/response block, and parcels
//! are encoded with [`libmessenger`]. The split `begin_call` + `await_reply`
//! ops are exposed too, because the kernel transaction already supports them
//! (`channels::begin_call`); the async API builds on the same pair later.
//!
//! The ABI blocks below mirror `kernel/src/ipc/syscalls.rs` field for field;
//! keep them in lockstep (the kernel pins the sizes at compile time).
//!
//! Deadlines are absolute PIT ticks (100 Hz), kernel-relative: `None` (encoded
//! as 0) waits forever, and userspace has no clock syscall yet, so callers that
//! want a timeout will need one before it is useful.

use alloc::vec;
use alloc::vec::Vec;

use libmessenger::Error as ParcelError;
/// Re-export the wire message type: every service helper above returns or
/// accepts parcels, so callers need the type by name.
pub use libmessenger::Parcel;

use crate::sys;

/// Native syscall ops, matching the kernel's `ipc::syscalls::OP_*`.
pub mod op {
    /// Call a method and block until the reply arrives.
    pub const CALL: u64 = 1;
    /// Answer a pending transaction with a reply parcel.
    pub const REPLY: u64 = 2;
    /// Send a one-way message; never blocks.
    pub const SEND: u64 = 3;
    /// Receive the next message, blocking until one is queued.
    pub const RECV: u64 = 4;
    /// Cancel a pending transaction.
    pub const CANCEL: u64 = 5;
    /// Close an endpoint handle.
    pub const CLOSE_ENDPOINT: u64 = 6;
    /// Create a fresh channel pair; both handles open in this task.
    pub const CREATE_PAIR: u64 = 7;
    /// Read channel counters (`handle = 0` means every live channel).
    pub const STATS: u64 = 8;
    /// Claim the boot-time client endpoint (first userspace task only).
    pub const BOOTSTRAP: u64 = 9;
    /// Register a call and park, returning the transaction id.
    pub const CALL_BEGIN: u64 = 10;
    /// Wait for a `CALL_BEGIN` transaction and return its reply.
    pub const CALL_AWAIT: u64 = 11;
    /// Global message totals in the compact 64-byte [`Stats`] shape.
    pub const TOTALS: u64 = 12;
    /// Publish a service name in the kernel registry (issue #89).
    pub const REGISTER: u64 = 13;
    /// Resolve a service name to a new handle.
    pub const RESOLVE: u64 = 14;
    /// Withdraw a service name.
    pub const UNREGISTER: u64 = 15;
    /// Snapshot the name table into the caller's buffer.
    pub const LIST: u64 = 16;
    /// Ask the kernel policy engine about every segment of a topic or filter
    /// (issue #92); the daemon uses this on behalf of a requesting client.
    pub const AUTHORIZE_TOPIC: u64 = 17;
}

/// `MsgArgs::txn_id` marker for registry ops: act on the calling task. A
/// different slot is the `messengerd` proxy path (kernel-side
/// `CAP_IPC_CONTROL`).
pub const REGISTRY_TARGET_SELF: u64 = u64::MAX;

/// Negative errno values the kernel returns; see the kernel's
/// `ipc::syscalls::errno`.
pub mod errno {
    pub const EPERM: i64 = 1;
    pub const ENOENT: i64 = 2;
    pub const E2BIG: i64 = 7;
    pub const EAGAIN: i64 = 11;
    pub const ENOMEM: i64 = 12;
    pub const EACCES: i64 = 13;
    pub const EFAULT: i64 = 14;
    pub const EBUSY: i64 = 16;
    pub const EEXIST: i64 = 17;
    pub const EINVAL: i64 = 22;
    pub const EPIPE: i64 = 32;
    pub const EDEADLK: i64 = 35;
    pub const ETIMEDOUT: i64 = 110;
    pub const ECANCELED: i64 = 125;
}

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
    /// Reserved; always zero today.
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
pub const FABRIC_TASKS: usize = 16;

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

/// The versioned fabric snapshot (stats ABI version 2): channels, messages,
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
    /// The ABI version this mirror understands.
    pub const VERSION: u64 = 2;
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
            Error::Registry(code) | Error::Topics(code) => Some(-code),
            Error::Parcel(_) => None,
        }
    }

    /// A short, human-readable explanation (friendly-errors convention).
    pub fn message(self) -> &'static str {
        match self {
            Error::Parcel(error) => error.message(),
            Error::Registry(code) => registry_message(code),
            Error::Topics(code) => topics_message(code),
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

/// One end of a Messenger channel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Endpoint {
    handle: u64,
}

impl Endpoint {
    /// Wrap a raw handle number (e.g. one carried inside a parcel).
    pub const fn from_raw(handle: u64) -> Endpoint {
        Endpoint { handle }
    }

    /// The underlying handle number.
    pub const fn handle(self) -> u64 {
        self.handle
    }

    /// Blocking call: encode `request`, wait for the reply, decode it.
    ///
    /// The reply buffer is allocated per call; a long-lived service loop must
    /// use [`Endpoint::call_with`] instead, because the user runtime's bump
    /// allocator never reclaims these buffers (see `user/src/heap.rs`).
    pub fn call(&self, request: &Parcel, deadline: Option<u64>) -> Result<Parcel> {
        let mut buf = vec![0u8; DEFAULT_BUFFER];
        self.call_with(request, &mut buf, deadline)
    }

    /// [`Endpoint::call`] with a caller-owned reply buffer, for loops that
    /// cannot afford a fresh allocation per message.
    pub fn call_with(
        &self,
        request: &Parcel,
        buf: &mut [u8],
        deadline: Option<u64>,
    ) -> Result<Parcel> {
        let bytes = encode(request)?;
        let args = MsgArgs {
            handle: self.handle,
            parcel_ptr: bytes.as_ptr() as u64,
            parcel_len: bytes.len() as u64,
            buf_ptr: buf.as_mut_ptr() as u64,
            buf_cap: buf.len() as u64,
            deadline: deadline.unwrap_or(0),
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        syscall(op::CALL, &args, &mut result)?;
        let len = result.bytes as usize;
        if len > buf.len() {
            return Err(Error::Errno(-errno::E2BIG));
        }
        Parcel::decode(&buf[..len]).map_err(Error::Parcel)
    }

    /// Register a call and park the task; returns the transaction id. Complete
    /// it with [`Endpoint::await_reply`] â€” the same split the kernel uses for
    /// asynchronous completion.
    pub fn begin_call(&self, request: &Parcel, deadline: Option<u64>) -> Result<u64> {
        let bytes = encode(request)?;
        let args = MsgArgs {
            handle: self.handle,
            parcel_ptr: bytes.as_ptr() as u64,
            parcel_len: bytes.len() as u64,
            deadline: deadline.unwrap_or(0),
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        syscall(op::CALL_BEGIN, &args, &mut result)?;
        Ok(result.value)
    }

    /// Wait for a [`Endpoint::begin_call`] transaction and decode its reply.
    pub fn await_reply(&self, txn_id: u64) -> Result<Parcel> {
        let mut buf = vec![0u8; DEFAULT_BUFFER];
        let args = MsgArgs {
            txn_id,
            buf_ptr: buf.as_mut_ptr() as u64,
            buf_cap: buf.len() as u64,
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        syscall(op::CALL_AWAIT, &args, &mut result)?;
        let len = result.bytes as usize;
        if len > buf.len() {
            return Err(Error::Errno(-errno::E2BIG));
        }
        Parcel::decode(&buf[..len]).map_err(Error::Parcel)
    }

    /// Answer a pending transaction with `reply` (the server side of a call).
    pub fn reply(&self, txn_id: u64, reply: &Parcel) -> Result<()> {
        let bytes = encode(reply)?;
        let args = MsgArgs {
            txn_id,
            parcel_ptr: bytes.as_ptr() as u64,
            parcel_len: bytes.len() as u64,
            ..MsgArgs::default()
        };
        syscall(op::REPLY, &args, &mut MsgResult::default())
    }

    /// Fire-and-forget send; returns once the message is queued.
    pub fn send(&self, parcel: &Parcel) -> Result<()> {
        let bytes = encode(parcel)?;
        let args = MsgArgs {
            handle: self.handle,
            parcel_ptr: bytes.as_ptr() as u64,
            parcel_len: bytes.len() as u64,
            ..MsgArgs::default()
        };
        syscall(op::SEND, &args, &mut MsgResult::default())
    }

    /// Block until a message arrives (or the deadline passes).
    ///
    /// Allocates the receive buffer per call; a service loop should use
    /// [`Endpoint::recv_with`] and reuse one buffer.
    pub fn recv(&self, deadline: Option<u64>) -> Result<Message> {
        let mut buf = vec![0u8; DEFAULT_BUFFER];
        self.recv_into(&mut buf, deadline)
    }

    /// [`Endpoint::recv`] into a caller-provided buffer.
    ///
    /// The userspace heap is a bump allocator that never frees, so a polling
    /// loop can avoid a fresh [`DEFAULT_BUFFER`] per iteration by reusing one
    /// scratch buffer here. A message larger than `buf` is refused with
    /// `-E2BIG` after delivery, exactly like [`Endpoint::recv`].
    pub fn recv_into(&self, buf: &mut [u8], deadline: Option<u64>) -> Result<Message> {
        let args = MsgArgs {
            handle: self.handle,
            buf_ptr: buf.as_mut_ptr() as u64,
            buf_cap: buf.len() as u64,
            deadline: deadline.unwrap_or(0),
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        syscall(op::RECV, &args, &mut result)?;
        let len = result.bytes as usize;
        if len > buf.len() {
            return Err(Error::Errno(-errno::E2BIG));
        }
        let parcel = Parcel::decode(&buf[..len]).map_err(Error::Parcel)?;
        Ok(Message {
            sender: result.aux,
            txn: (result.value != 0).then_some(result.value),
            parcel,
        })
    }

    /// Alias for [`Endpoint::recv_into`], kept for the supervisor services.
    pub fn recv_with(&self, buf: &mut [u8], deadline: Option<u64>) -> Result<Message> {
        self.recv_into(buf, deadline)
    }

    /// Poll for a queued message without blocking (issue #91).
    ///
    /// `Ok(None)` means "nothing queued yet"; a dead peer still reports
    /// `-EPIPE`. The kernel checks the inbox first, so a queued message is
    /// returned immediately; otherwise the call parks with an already-expired
    /// deadline ([`EXPIRED_DEADLINE`]) and wakes with `-ETIMEDOUT` on the first
    /// timer gate. Before the PIT's first tick that wake can take up to 10 ms,
    /// and a busy runnable peer may be scheduled before this task resumes.
    pub fn poll_recv(&self) -> Result<Option<Message>> {
        let mut buf = vec![0u8; DEFAULT_BUFFER];
        self.poll_recv_with(&mut buf)
    }

    /// [`Endpoint::poll_recv`] with a caller-owned buffer.
    pub fn poll_recv_with(&self, buf: &mut [u8]) -> Result<Option<Message>> {
        match self.recv_with(buf, Some(EXPIRED_DEADLINE)) {
            Ok(message) => Ok(Some(message)),
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Cancel a pending transaction started by this task.
    pub fn cancel(&self, txn_id: u64) -> Result<()> {
        let args = MsgArgs {
            txn_id,
            ..MsgArgs::default()
        };
        syscall(op::CANCEL, &args, &mut MsgResult::default())
    }

    /// Close this endpoint; the channel dies when both ends close.
    pub fn close(self) -> Result<()> {
        let args = MsgArgs {
            handle: self.handle,
            ..MsgArgs::default()
        };
        syscall(op::CLOSE_ENDPOINT, &args, &mut MsgResult::default())
    }

    /// Counters for this endpoint's channel.
    pub fn stats(&self) -> Result<Stats> {
        stats_call(self.handle)
    }
}

/// A received message: the decoded parcel plus the kernel-stamped metadata
/// userspace cannot otherwise see.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Message {
    /// Task slot that sent the message.
    pub sender: u64,
    /// Kernel transaction id for a call; `None` for one-way messages.
    pub txn: Option<u64>,
    /// The decoded parcel.
    pub parcel: Parcel,
}

impl Message {
    /// Method id from the parcel header.
    pub fn method(&self) -> u32 {
        self.parcel.header.method
    }

    /// Interface id from the parcel header.
    pub fn interface_id(&self) -> u64 {
        self.parcel.header.interface_id
    }
}

/// Claim the boot-time client endpoint. Only the first userspace task can; a
/// later caller gets `-EBUSY`.
pub fn bootstrap() -> Result<Endpoint> {
    let mut result = MsgResult::default();
    syscall(op::BOOTSTRAP, &MsgArgs::default(), &mut result)?;
    Ok(Endpoint::from_raw(result.value))
}

/// Create a fresh channel pair; both endpoints open in this task.
pub fn create_pair() -> Result<(Endpoint, Endpoint)> {
    let mut result = MsgResult::default();
    syscall(op::CREATE_PAIR, &MsgArgs::default(), &mut result)?;
    Ok((
        Endpoint::from_raw(result.value),
        Endpoint::from_raw(result.aux),
    ))
}

/// Aggregated counters across every live channel (compact shape).
pub fn global_stats() -> Result<Stats> {
    stats_call(0)
}

/// Global message totals through the dedicated `TOTALS` op.
pub fn global_totals() -> Result<Stats> {
    let mut stats = Stats::default();
    let args = MsgArgs {
        buf_ptr: &mut stats as *mut Stats as u64,
        buf_cap: Stats::SIZE as u64,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    syscall(op::TOTALS, &args, &mut result)?;
    if result.bytes as usize != Stats::SIZE {
        return Err(Error::Errno(-errno::E2BIG));
    }
    Ok(stats)
}

/// The versioned fabric snapshot (stats ABI v2): every subsystem in one block.
/// The snapshot buffer is sized so the kernel always serves version 2.
///
/// Allocates the snapshot buffer per call; a polling loop should use
/// [`fabric_stats_with`] and reuse one buffer.
pub fn fabric_stats() -> Result<FabricStats> {
    let mut buf = vec![0u8; FabricStats::SIZE];
    fabric_stats_with(&mut buf)
}

/// [`fabric_stats`] with a caller-owned buffer of at least
/// [`FabricStats::SIZE`] bytes.
pub fn fabric_stats_with(buf: &mut [u8]) -> Result<FabricStats> {
    let args = MsgArgs {
        buf_ptr: buf.as_mut_ptr() as u64,
        buf_cap: buf.len() as u64,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    syscall(op::STATS, &args, &mut result)?;
    let len = result.bytes as usize;
    if len > buf.len() {
        return Err(Error::Errno(-errno::E2BIG));
    }
    FabricStats::from_bytes(&buf[..len]).ok_or(Error::Errno(-errno::EINVAL))
}

/// A blocking request loop in the `Server::serve` shape.
pub struct Server {
    endpoint: Endpoint,
}

impl Server {
    /// Wrap an endpoint that is being served.
    pub const fn new(endpoint: Endpoint) -> Server {
        Server { endpoint }
    }

    /// Receive one request, dispatch it to `handler`, and reply when the
    /// message was a call. One-way messages are passed to the handler too, but
    /// their (ignored) reply parcel is not sent.
    pub fn serve_once<F>(&self, handler: &mut F) -> Result<()>
    where
        F: FnMut(&Message) -> Result<Parcel>,
    {
        let message = self.endpoint.recv(None)?;
        let reply = handler(&message)?;
        if let Some(txn) = message.txn {
            self.endpoint.reply(txn, &reply)?;
        }
        Ok(())
    }

    /// [`Server::serve_once`] forever.
    pub fn serve<F>(&self, mut handler: F) -> Result<()>
    where
        F: FnMut(&Message) -> Result<Parcel>,
    {
        loop {
            self.serve_once(&mut handler)?;
        }
    }
}

/// One `messenger` syscall; a negative return is an errno.
fn syscall(op: u64, args: &MsgArgs, result: &mut MsgResult) -> Result<()> {
    let code = sys::messenger(
        op,
        args as *const MsgArgs as u64,
        result as *mut MsgResult as u64,
    );
    if code < 0 {
        Err(Error::Errno(code))
    } else {
        Ok(())
    }
}

/// Encode a parcel for the wire.
fn encode(parcel: &Parcel) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(Error::Parcel)?;
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Service name registry (issue #89)
// ---------------------------------------------------------------------------

/// Service name registry: the clients' and the daemon's view of the kernel
/// table (`docs/messenger.md` section 8).
///
/// Two paths reach the same table:
///
/// * the **direct** functions ([`register`], [`resolve`], [`unregister`],
///   [`list`]) are the native `register`/`resolve`/`unregister`/`list` ops; the
///   calling task is the owner, and `resolve` opens the discovered endpoint in
///   the caller's own handle table;
/// * [`Client`] talks to `messengerd` over the bootstrap channel. The daemon
///   is a thin, privileged proxy: it receives the request, forwards it to the
///   kernel with the *requester's* slot as the target (the kernel opens the
///   resolved handle straight into the requester's table), and answers with the
///   result or a friendly error.
///
/// [`serve_request`] is the daemon's half of that protocol: `messengerd` hands
/// it each received parcel and the kernel-stamped sender slot, and sends the
/// returned parcel back as the reply.
pub mod registry {
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

    use super::{
        errno, op, syscall, Endpoint, Error, MsgArgs, MsgResult, Result, REGISTRY_TARGET_SELF,
    };

    /// The bootstrap listener's well-known name. The kernel publishes it at
    /// boot; it is the one name every task can resolve.
    pub const NAME: &str = "os.lazy.messenger.registry";

    /// Registry interface id: the first eight bytes of the spec name
    /// `os.lazy.messenger.registry.v1`, mirroring `kernel/src/ipc/registry.rs`.
    pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.lazy.");

    /// Registry methods, mirroring `kernel/src/ipc/registry.rs`.
    pub mod method {
        pub const REGISTER: u32 = 1;
        pub const RESOLVE: u32 = 2;
        pub const UNREGISTER: u32 = 3;
        pub const LIST: u32 = 4;
    }

    /// Registry TLV field ids, mirroring `kernel/src/ipc/registry.rs`.
    pub mod field {
        pub const NAME: u16 = 1;
        pub const INTERFACES: u16 = 2;
        pub const LEASE_TICKS: u16 = 3;
        pub const ENDPOINT: u16 = 4;
        pub const OBJECT: u16 = 5;
        pub const OWNER: u16 = 6;
        pub const LEASE_REMAINING: u16 = 7;
        pub const ENTRY: u16 = 8;
        pub const HANDLE: u16 = 9;
        /// Daemon protocol only: a structured error reply.
        pub const ERROR: u16 = 10;
    }

    /// Largest `List` reply the client offers the kernel. The table holds at
    /// most 64 names of 128 bytes, so 32 KiB has generous room.
    pub const LIST_BUFFER: usize = 32 * 1024;

    /// One registered name, decoded from a list reply.
    #[derive(Clone, PartialEq, Eq, Debug)]
    pub struct Entry {
        /// Service name.
        pub name: String,
        /// Kernel object the name refers to (diagnostic).
        pub object_id: u64,
        /// Task slot that owns the name.
        pub owner_slot: u64,
        /// Interface ids the service implements.
        pub interfaces: Vec<u64>,
        /// Remaining lease ticks; `0` when permanent.
        pub lease_remaining: u64,
    }

    /// A header for a registry parcel of `method`.
    ///
    /// `ALLOW_NESTED` is required on the shared bootstrap channel: a topic
    /// subscriber may be parked in `next_event` (a pending transaction on the
    /// same channel) while another task resolves a name, and the kernel's
    /// per-channel cycle check would otherwise refuse the resolve.
    fn header(method: u32) -> Header {
        Header {
            version: VERSION,
            flags: libmessenger::flags::ALLOW_NESTED,
            interface_id: INTERFACE,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        }
    }

    /// Wrap an encoded body in a registry parcel.
    fn request_parcel(method: u32, body: Encoder) -> Parcel {
        Parcel {
            header: header(method),
            body: body.finish(),
            handles: Vec::new(),
            buffers: Vec::new(),
        }
    }

    /// A body carrying `name` (the common request shape).
    fn name_body(name: &str) -> Result<Encoder> {
        let mut body = Encoder::new();
        body.string(field::NAME, name).map_err(Error::Parcel)?;
        Ok(body)
    }

    /// A register body: name, endpoint handle, interface array, lease.
    fn register_body(
        name: &str,
        endpoint: u64,
        interfaces: &[u64],
        lease_ticks: u64,
    ) -> Result<Encoder> {
        let mut body = name_body(name)?;
        body.u64(field::ENDPOINT, endpoint).map_err(Error::Parcel)?;
        let mut array = Encoder::new();
        for interface in interfaces {
            array
                .u64(field::INTERFACES, *interface)
                .map_err(Error::Parcel)?;
        }
        body.array(field::INTERFACES, &array)
            .map_err(Error::Parcel)?;
        body.u64(field::LEASE_TICKS, lease_ticks)
            .map_err(Error::Parcel)?;
        Ok(body)
    }

    /// Encode a parcel for the wire.
    fn encode(parcel: &Parcel) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        parcel.encode(&mut bytes).map_err(Error::Parcel)?;
        Ok(bytes)
    }

    /// Run one registry request through the native gate with `target` as the
    /// task whose table the operation touches.
    fn registry_call(op_code: u64, target: u64, parcel: &Parcel) -> Result<MsgResult> {
        let bytes = encode(parcel)?;
        let args = MsgArgs {
            txn_id: target,
            parcel_ptr: bytes.as_ptr() as u64,
            parcel_len: bytes.len() as u64,
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        syscall(op_code, &args, &mut result)?;
        Ok(result)
    }

    /// Register `endpoint` in the table on behalf of `target`'s task.
    ///
    /// The direct path passes [`REGISTRY_TARGET_SELF`]; [`serve_request`]
    /// passes the requester's slot and the kernel checks the proxy capability.
    fn register_for(
        target: u64,
        name: &str,
        endpoint: u64,
        interfaces: &[u64],
        lease_ticks: u64,
    ) -> Result<()> {
        let parcel = request_parcel(
            method::REGISTER,
            register_body(name, endpoint, interfaces, lease_ticks)?,
        );
        registry_call(op::REGISTER, target, &parcel)?;
        Ok(())
    }

    /// Publish `endpoint` under `name`; the caller becomes the owner. A
    /// `lease_ticks` of `0` registers a permanent name.
    pub fn register(
        name: &str,
        endpoint: &Endpoint,
        interfaces: &[u64],
        lease_ticks: u64,
    ) -> Result<()> {
        register_for(
            REGISTRY_TARGET_SELF,
            name,
            endpoint.handle(),
            interfaces,
            lease_ticks,
        )
    }

    /// Resolve `name` into `target`'s table and return the new handle.
    fn resolve_for(target: u64, name: &str) -> Result<Endpoint> {
        let parcel = request_parcel(method::RESOLVE, name_body(name)?);
        let result = registry_call(op::RESOLVE, target, &parcel)?;
        Ok(Endpoint::from_raw(result.value))
    }

    /// Resolve `name`; the returned endpoint is open in this task's table.
    pub fn resolve(name: &str) -> Result<Endpoint> {
        resolve_for(REGISTRY_TARGET_SELF, name)
    }

    /// Withdraw `name` on behalf of `target`'s task.
    fn unregister_for(target: u64, name: &str) -> Result<()> {
        let parcel = request_parcel(method::UNREGISTER, name_body(name)?);
        registry_call(op::UNREGISTER, target, &parcel)?;
        Ok(())
    }

    /// Withdraw `name`. Only its owner (or an administrator) may.
    pub fn unregister(name: &str) -> Result<()> {
        unregister_for(REGISTRY_TARGET_SELF, name)
    }

    /// Snapshot the name table.
    pub fn list() -> Result<Vec<Entry>> {
        let mut buf = vec![0u8; LIST_BUFFER];
        let args = MsgArgs {
            buf_ptr: buf.as_mut_ptr() as u64,
            buf_cap: buf.len() as u64,
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        syscall(op::LIST, &args, &mut result)?;
        let len = result.bytes as usize;
        if len > buf.len() {
            return Err(Error::Errno(-errno::E2BIG));
        }
        let parcel = Parcel::decode(&buf[..len]).map_err(Error::Parcel)?;
        decode_entries(&parcel)
    }

    /// The first string field with `id`.
    fn string_field(parcel: &Parcel, id: u16) -> Result<String> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind == Kind::String && field.id == id {
                return Ok(String::from(field.as_str().map_err(Error::Parcel)?));
            }
        }
        Err(Error::Errno(-errno::EINVAL))
    }

    /// The first `u64` field with `id`, if any.
    fn u64_field(parcel: &Parcel, id: u16) -> Result<Option<u64>> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind == Kind::U64 && field.id == id {
                return Ok(Some(field.as_u64().map_err(Error::Parcel)?));
            }
        }
        Ok(None)
    }

    /// Decode the interface id array.
    fn interfaces_field(parcel: &Parcel) -> Result<Vec<u64>> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind == Kind::Array && field.id == field::INTERFACES {
                let mut nested = field.nested(0).map_err(Error::Parcel)?;
                let mut interfaces = Vec::new();
                while let Some(item) = nested.next().map_err(Error::Parcel)? {
                    if item.kind == Kind::U64 {
                        interfaces.push(item.as_u64().map_err(Error::Parcel)?);
                    }
                }
                return Ok(interfaces);
            }
        }
        Ok(Vec::new())
    }

    /// Decode a list reply body into entries; unknown fields are skipped so a
    /// newer kernel stays compatible with this client.
    fn decode_entries(parcel: &Parcel) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(record) = decoder.next().map_err(Error::Parcel)? {
            if record.kind != Kind::Struct || record.id != field::ENTRY {
                continue;
            }
            let mut nested = record.nested(0).map_err(Error::Parcel)?;
            let mut entry = Entry {
                name: String::new(),
                object_id: 0,
                owner_slot: 0,
                interfaces: Vec::new(),
                lease_remaining: 0,
            };
            while let Some(item) = nested.next().map_err(Error::Parcel)? {
                match (item.kind, item.id) {
                    (Kind::String, field::NAME) => {
                        entry.name = String::from(item.as_str().map_err(Error::Parcel)?);
                    }
                    (Kind::U64, field::OBJECT) => {
                        entry.object_id = item.as_u64().map_err(Error::Parcel)?;
                    }
                    (Kind::U64, field::OWNER) => {
                        entry.owner_slot = item.as_u64().map_err(Error::Parcel)?;
                    }
                    (Kind::U64, field::LEASE_REMAINING) => {
                        entry.lease_remaining = item.as_u64().map_err(Error::Parcel)?;
                    }
                    (Kind::Array, field::INTERFACES) => {
                        let mut array = item.nested(0).map_err(Error::Parcel)?;
                        while let Some(id) = array.next().map_err(Error::Parcel)? {
                            if id.kind == Kind::U64 {
                                entry.interfaces.push(id.as_u64().map_err(Error::Parcel)?);
                            }
                        }
                    }
                    _ => {}
                }
            }
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Encode entries into a list reply parcel (the daemon's `List` answer and
    /// the tests reuse this so both directions share one format).
    fn encode_entries(entries: &[Entry]) -> Result<Parcel> {
        let mut body = Encoder::new();
        for entry in entries {
            let mut record = Encoder::new();
            record
                .string(field::NAME, &entry.name)
                .map_err(Error::Parcel)?;
            record
                .u64(field::OBJECT, entry.object_id)
                .map_err(Error::Parcel)?;
            record
                .u64(field::OWNER, entry.owner_slot)
                .map_err(Error::Parcel)?;
            let mut interfaces = Encoder::new();
            for interface in &entry.interfaces {
                interfaces
                    .u64(field::INTERFACES, *interface)
                    .map_err(Error::Parcel)?;
            }
            record
                .array(field::INTERFACES, &interfaces)
                .map_err(Error::Parcel)?;
            record
                .u64(field::LEASE_REMAINING, entry.lease_remaining)
                .map_err(Error::Parcel)?;
            body.record(field::ENTRY, &record).map_err(Error::Parcel)?;
        }
        Ok(request_parcel(method::LIST, body))
    }

    /// The daemon's request handler: forward one registry request to the kernel
    /// on behalf of `sender` (the kernel-stamped task slot), and build the
    /// reply parcel.
    ///
    /// The endpoint handle in a `Register` request is a number in the sender's
    /// table; the kernel reads it there, so nothing crosses tables here.
    pub fn serve_request(request: &Parcel, sender: u64) -> Result<Parcel> {
        match request.header.method {
            method::REGISTER => {
                let name = string_field(request, field::NAME)?;
                let endpoint =
                    u64_field(request, field::ENDPOINT)?.ok_or(Error::Errno(-errno::EINVAL))?;
                let interfaces = interfaces_field(request)?;
                let lease = u64_field(request, field::LEASE_TICKS)?.unwrap_or(0);
                register_for(sender, &name, endpoint, &interfaces, lease)?;
                Ok(request_parcel(method::REGISTER, Encoder::new()))
            }
            method::RESOLVE => {
                let name = string_field(request, field::NAME)?;
                let endpoint = resolve_for(sender, &name)?;
                let mut body = Encoder::new();
                body.u64(field::HANDLE, endpoint.handle())
                    .map_err(Error::Parcel)?;
                Ok(request_parcel(method::RESOLVE, body))
            }
            method::UNREGISTER => {
                let name = string_field(request, field::NAME)?;
                unregister_for(sender, &name)?;
                Ok(request_parcel(method::UNREGISTER, Encoder::new()))
            }
            method::LIST => {
                let entries = list()?;
                encode_entries(&entries)
            }
            _ => Err(Error::Errno(-errno::EINVAL)),
        }
    }

    /// The daemon's error answer: the errno-style code plus the friendly text,
    /// so the client can return a [`Error::Registry`] with a readable message.
    pub fn error_reply(method: u32, error: Error) -> Parcel {
        let code = error.errno().map(|code| -code).unwrap_or(errno::EINVAL);
        let mut body = Encoder::new();
        // A structured error field cannot overflow a fresh encoder here.
        let _ = body.error(field::ERROR, code as u32, error.message());
        request_parcel(method, body)
    }

    /// The first structured error field, when the reply is a daemon failure.
    fn error_field(parcel: &Parcel) -> Result<Option<i64>> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind == Kind::Error && field.id == field::ERROR {
                let (code, _message) = field.error_parts().map_err(Error::Parcel)?;
                return Ok(Some(code as i64));
            }
        }
        Ok(None)
    }

    /// A client of the `messengerd` daemon over the bootstrap channel.
    pub struct Client {
        endpoint: Endpoint,
    }

    impl Client {
        /// Resolve the well-known registry name and wrap its endpoint.
        pub fn connect() -> Result<Client> {
            Ok(Client {
                endpoint: resolve(NAME)?,
            })
        }

        /// The underlying daemon endpoint (diagnostics).
        pub fn endpoint(&self) -> Endpoint {
            self.endpoint
        }

        /// Run one request as a blocking call and fail on a daemon error reply.
        fn call(&self, method: u32, body: Encoder) -> Result<Parcel> {
            let reply = self.endpoint.call(&request_parcel(method, body), None)?;
            if let Some(code) = error_field(&reply)? {
                return Err(Error::Registry(code));
            }
            Ok(reply)
        }

        /// Register `endpoint` under `name` through the daemon. The daemon
        /// forwards the request with this task as the owner.
        pub fn register(
            &self,
            name: &str,
            endpoint: &Endpoint,
            interfaces: &[u64],
            lease_ticks: u64,
        ) -> Result<()> {
            self.call(
                method::REGISTER,
                register_body(name, endpoint.handle(), interfaces, lease_ticks)?,
            )?;
            Ok(())
        }

        /// Resolve `name` through the daemon; the returned endpoint is open in
        /// this task's table.
        pub fn resolve(&self, name: &str) -> Result<Endpoint> {
            let reply = self.call(method::RESOLVE, name_body(name)?)?;
            let handle = u64_field(&reply, field::HANDLE)?.ok_or(Error::Errno(-errno::EINVAL))?;
            Ok(Endpoint::from_raw(handle))
        }

        /// Withdraw `name` through the daemon.
        pub fn unregister(&self, name: &str) -> Result<()> {
            self.call(method::UNREGISTER, name_body(name)?)?;
            Ok(())
        }

        /// Snapshot the table through the daemon.
        pub fn list(&self) -> Result<Vec<Entry>> {
            let reply = self.call(method::LIST, Encoder::new())?;
            decode_entries(&reply)
        }
    }
}

// ---------------------------------------------------------------------------
// Pub/sub topics (issue #92)
// ---------------------------------------------------------------------------

/// Publish/subscribe topics (`docs/messenger.md` section 7.2).
///
/// ## Where the broker lives
///
/// Topics live in **userspace**, in `messengerd`, per the epic decision
/// recorded in section 20: the kernel's job is policy and message transport,
/// not naming, filters, QoS or retained state. The broker is addressed through
/// the well-known name [`NAME`] on the same bootstrap endpoint as the service
/// registry â€” the daemon dispatches on the parcel's interface id.
///
/// ## Delivery is pull-based, with kernel-mediated blocking
///
/// A subscription is a broker-side id, not a channel or a handle. The
/// subscriber asks for the next event with [`Subscription::next_event`], a
/// synchronous call to the broker:
///
/// * when an event is queued the broker replies immediately;
/// * when the queue is empty the broker **parks the transaction** and answers
///   it later, when a matching `Publish` arrives. The subscriber sleeps in the
///   kernel's wait queue with a real deadline, so `next_event(Some(ticks))`
///   times out cleanly and a slow subscriber never stalls the publisher.
///
/// This is why the broker's calls carry [`libmessenger::flags::ALLOW_NESTED`]:
/// the bootstrap channel is shared by every client, and one subscribed task
/// may be parked in `next_event` while another publishes on the same channel.
/// The kernel's per-channel cycle check would otherwise refuse the second
/// call as a deadlock.
///
/// ## QoS
///
/// [`Qos::Latest`] keeps one event (new replaces old), [`Qos::Buffered`] keeps
/// `N` and drops the oldest on overflow, [`Qos::Conflate`] coalesces the latest
/// event per publisher in its window, and [`Qos::Reliable`] keeps events until
/// the subscriber [`Subscription::ack`]s them, redelivering the head on the
/// next request. The broker counts every dropped event per subscriber;
/// [`Subscription::stats`] exposes the counter. There are no timers in
/// userspace yet, so `reliable` retirement is pull-driven (an event stays
/// outstanding until acked or the subscriber dies) â€” best-effort after peer
/// death is the documented limit.
///
/// ## Policy
///
/// Every publish and subscribe is checked segment by segment through the
/// kernel (`op::AUTHORIZE_TOPIC`), so `ipc::authorize` and its audit ring stay
/// the single policy choke point; the broker only maps `-EACCES` to its
/// friendly denial reply.
pub mod topics_client {
    use alloc::string::String;
    use alloc::vec::Vec;

    use libmessenger::{flags, Decoder, Encoder, Header, Kind, Parcel, VERSION};

    use super::{
        encode, errno, op, registry, syscall, Endpoint, Error, MsgArgs, MsgResult, Result,
        EXPIRED_DEADLINE,
    };

    /// Well-known broker name; `messengerd` registers it at startup.
    pub const NAME: &str = "os.lazy.messenger.topics";

    /// Topics interface id: `fnv1a64("os.lazy.messenger.topics.v1")`, the same
    /// `tools/midlc` hash the kernel and broker use.
    pub const INTERFACE: u64 = 0xc573_4f97_8fef_7231;

    /// Broker method ids (`fnv1a32` of the method name, `tools/midlc` style).
    pub mod method {
        /// Publish one payload under a topic.
        pub const PUBLISH: u32 = 1818372520;
        /// Create a subscription for a filter.
        pub const SUBSCRIBE: u32 = 6992035;
        /// Drop a subscription.
        pub const UNSUBSCRIBE: u32 = 2099666486;
        /// Wait for (or poll) the next event of a subscription.
        pub const NEXT_EVENT: u32 = 1278354512;
        /// Retire an event delivered by a `reliable` subscription.
        pub const ACK: u32 = 483717538;
        /// List topics the broker has seen.
        pub const LIST_TOPICS: u32 = 225427937;
        /// Per-subscription queue and drop counters.
        pub const STATS: u32 = 788260383;
        /// Round-trip probe used to detect a live broker.
        pub const PING: u32 = 2142761129;
    }

    /// TLV field ids of the broker protocol.
    pub mod field {
        /// Publish topic / event topic.
        pub const TOPIC: u16 = 1;
        /// Subscription filter.
        pub const FILTER: u16 = 2;
        /// Encoded payload parcel bytes.
        pub const PAYLOAD: u16 = 3;
        /// Whether a publish is the retained value.
        pub const RETAINED: u16 = 4;
        /// QoS code.
        pub const QOS: u16 = 5;
        /// Buffered depth.
        pub const DEPTH: u16 = 6;
        /// Subscription id.
        pub const SUBSCRIPTION: u16 = 7;
        /// Event sequence / ack sequence.
        pub const SEQUENCE: u16 = 8;
        /// Publisher task slot.
        pub const PUBLISHER: u16 = 9;
        /// Nested event record.
        pub const EVENT: u16 = 10;
        /// Subscribers a publish matched.
        pub const MATCHED: u16 = 11;
        /// Dropped events (subscription stats).
        pub const DROPS: u16 = 12;
        /// Queued events (subscription stats).
        pub const QUEUED: u16 = 13;
        /// Delivered events (subscription stats).
        pub const DELIVERED: u16 = 14;
        /// Subscribers matching a listed topic.
        pub const SUBSCRIBERS: u16 = 15;
        /// Nested topic record.
        pub const ENTRY: u16 = 16;
        /// Structured error reply.
        pub const ERROR: u16 = 17;
    }

    /// TLV field ids of the kernel `authorize_topic` request; mirrors
    /// `kernel/src/ipc/topics.rs`.
    pub mod auth_field {
        pub const NAME: u16 = 1;
        pub const MODE: u16 = 2;
        pub const TXN: u16 = 3;
    }

    /// Kernel mode code for a publish ACL check.
    pub const MODE_PUBLISH: u32 = 0;
    /// Kernel mode code for a subscribe ACL check.
    pub const MODE_SUBSCRIBE: u32 = 1;

    /// Payload bytes accepted by the broker in one event. Sized well below the
    /// 16 KiB call buffer so a `NextEvent` reply always fits.
    pub const MAX_PAYLOAD: usize = 8 * 1024;

    /// PIT ticks [`Client::connect`] waits for the broker name to appear.
    /// `messengerd` registers [`NAME`] during its own boot and is spawned
    /// before its clients, so a handful of ticks is ample; a boot without a
    /// broker should still reach the prompt promptly. Userspace has no clock
    /// syscall yet, so each retry sleeps one tick by parking on a private
    /// channel pair with an expired deadline.
    const CONNECT_ATTEMPTS: usize = 8;

    /// Delivery contract chosen at subscribe time; enforced by the broker.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Qos {
        /// Keep only the most recent event; a replacement overwrites.
        Latest,
        /// Keep up to `N` events; overflow drops the oldest.
        Buffered(u32),
        /// Coalesce to the latest event per publisher until consumed.
        Conflate,
        /// Keep events until the subscriber acks them; bounded retry.
        Reliable,
    }

    impl Qos {
        /// Largest accepted `Buffered` depth.
        pub const MAX_DEPTH: u32 = 64;
        /// Queue depth a `Reliable` subscription gets.
        pub const RELIABLE_DEPTH: u32 = 8;
        /// Distinct publishers a `Conflate` subscription coalesces across.
        pub const CONFLATE_WINDOW: u32 = 4;

        /// The wire code.
        pub const fn code(self) -> u32 {
            match self {
                Qos::Latest => 0,
                Qos::Buffered(_) => 1,
                Qos::Conflate => 2,
                Qos::Reliable => 3,
            }
        }

        /// The effective queue depth (clamped to at least one).
        pub const fn depth(self) -> u32 {
            match self {
                Qos::Buffered(depth) => {
                    if depth == 0 {
                        1
                    } else if depth > Self::MAX_DEPTH {
                        Self::MAX_DEPTH
                    } else {
                        depth
                    }
                }
                Qos::Reliable => Self::RELIABLE_DEPTH,
                Qos::Conflate => Self::CONFLATE_WINDOW,
                Qos::Latest => 1,
            }
        }

        /// Decode `(code, depth)` from the wire, or `None` for an unknown code.
        pub fn from_parts(code: u32, depth: u32) -> Option<Qos> {
            match code {
                0 => Some(Qos::Latest),
                1 => Some(Qos::Buffered(depth)),
                2 => Some(Qos::Conflate),
                3 => Some(Qos::Reliable),
                _ => None,
            }
        }
    }

    /// One delivered event: broker metadata plus the publisher's opaque
    /// payload parcel.
    #[derive(Clone, PartialEq, Eq, Debug)]
    pub struct Event {
        /// Topic the event was published under.
        pub topic: String,
        /// Task slot of the publisher (kernel-stamped when the publish arrived).
        pub publisher: u64,
        /// Broker sequence number (monotonic per broker boot).
        pub sequence: u64,
        /// Whether this event is a retained value replay.
        pub retained: bool,
        /// The payload parcel, still encoded; decode with [`Event::parcel`].
        pub payload: Vec<u8>,
    }

    impl Event {
        /// Decode the stored payload into the parcel the publisher sent.
        pub fn parcel(&self) -> Result<Parcel> {
            Parcel::decode(&self.payload).map_err(Error::Parcel)
        }
    }

    /// Per-subscription delivery counters.
    #[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
    pub struct SubscriptionStats {
        /// QoS code the subscription was created with.
        pub qos: u32,
        /// Effective queue depth.
        pub depth: u32,
        /// Events currently queued (reliable: delivered but unacked included).
        pub queued: u64,
        /// Events handed to the subscriber.
        pub delivered: u64,
        /// Events the subscription's filter matched.
        pub matched: u64,
        /// Events dropped by the QoS policy or a full queue.
        pub drops: u64,
    }

    /// One row of [`Client::list`].
    #[derive(Clone, PartialEq, Eq, Debug)]
    pub struct TopicInfo {
        /// Topic name.
        pub topic: String,
        /// Live subscriptions whose filter matches it.
        pub subscribers: u64,
        /// Whether the broker holds a retained value for it.
        pub retained: bool,
    }

    /// A header for a broker parcel of `method`.
    ///
    /// `ALLOW_NESTED` is required, not optional: every client shares the
    /// daemon's bootstrap channel, so `next_event` may leave a transaction
    /// open while another task publishes (see the module docs).
    fn header(method: u32) -> Header {
        Header {
            version: VERSION,
            flags: flags::SYNC | flags::ALLOW_NESTED,
            interface_id: INTERFACE,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        }
    }

    /// Wrap an encoded body in a broker parcel.
    pub fn request_parcel(method: u32, body: Encoder) -> Parcel {
        Parcel {
            header: header(method),
            body: body.finish(),
            handles: Vec::new(),
            buffers: Vec::new(),
        }
    }

    /// An empty reply.
    pub fn reply_ok(method: u32) -> Parcel {
        request_parcel(method, Encoder::new())
    }

    /// A publish reply carrying the subscriber count the event reached.
    pub fn reply_matched(matched: u64) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::MATCHED, matched).map_err(Error::Parcel)?;
        Ok(request_parcel(method::PUBLISH, body))
    }

    /// A subscribe reply carrying the new subscription id.
    pub fn reply_subscription(id: u64) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::SUBSCRIPTION, id).map_err(Error::Parcel)?;
        Ok(request_parcel(method::SUBSCRIBE, body))
    }

    /// A `NextEvent` reply carrying one event.
    pub fn reply_event(event: &Event) -> Result<Parcel> {
        let mut record = Encoder::new();
        record
            .string(field::TOPIC, &event.topic)
            .map_err(Error::Parcel)?;
        record
            .u64(field::PUBLISHER, event.publisher)
            .map_err(Error::Parcel)?;
        record
            .u64(field::SEQUENCE, event.sequence)
            .map_err(Error::Parcel)?;
        record
            .bool(field::RETAINED, event.retained)
            .map_err(Error::Parcel)?;
        record
            .bytes(field::PAYLOAD, &event.payload)
            .map_err(Error::Parcel)?;
        let mut body = Encoder::new();
        body.record(field::EVENT, &record).map_err(Error::Parcel)?;
        Ok(request_parcel(method::NEXT_EVENT, body))
    }

    /// A stats reply.
    pub fn reply_stats(stats: &SubscriptionStats) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u32(field::QOS, stats.qos).map_err(Error::Parcel)?;
        body.u32(field::DEPTH, stats.depth).map_err(Error::Parcel)?;
        body.u64(field::QUEUED, stats.queued)
            .map_err(Error::Parcel)?;
        body.u64(field::DELIVERED, stats.delivered)
            .map_err(Error::Parcel)?;
        body.u64(field::MATCHED, stats.matched)
            .map_err(Error::Parcel)?;
        body.u64(field::DROPS, stats.drops).map_err(Error::Parcel)?;
        Ok(request_parcel(method::STATS, body))
    }

    /// A topic-list reply.
    pub fn reply_topics(topics: &[TopicInfo]) -> Result<Parcel> {
        let mut body = Encoder::new();
        for info in topics {
            let mut record = Encoder::new();
            record
                .string(field::TOPIC, &info.topic)
                .map_err(Error::Parcel)?;
            record
                .u64(field::SUBSCRIBERS, info.subscribers)
                .map_err(Error::Parcel)?;
            record
                .bool(field::RETAINED, info.retained)
                .map_err(Error::Parcel)?;
            body.record(field::ENTRY, &record).map_err(Error::Parcel)?;
        }
        Ok(request_parcel(method::LIST_TOPICS, body))
    }

    /// The broker's error answer: errno-style code plus friendly text.
    pub fn error_reply(method: u32, error: Error) -> Parcel {
        let code = error.errno().map(|code| -code).unwrap_or(errno::EINVAL);
        let mut body = Encoder::new();
        // A structured error field cannot overflow a fresh encoder here.
        let _ = body.error(field::ERROR, code as u32, error.message());
        request_parcel(method, body)
    }

    /// The first structured error field, when the reply is a broker failure.
    fn error_field(parcel: &Parcel) -> Result<Option<i64>> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind == Kind::Error && field.id == field::ERROR {
                let (code, _message) = field.error_parts().map_err(Error::Parcel)?;
                return Ok(Some(code as i64));
            }
        }
        Ok(None)
    }

    /// The first string field with `id`.
    pub fn string_field(parcel: &Parcel, id: u16) -> Result<String> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind == Kind::String && field.id == id {
                return Ok(String::from(field.as_str().map_err(Error::Parcel)?));
            }
        }
        Err(Error::Errno(-errno::EINVAL))
    }

    /// The first `u64` field with `id`, if any.
    pub fn u64_field(parcel: &Parcel, id: u16) -> Result<Option<u64>> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind == Kind::U64 && field.id == id {
                return Ok(Some(field.as_u64().map_err(Error::Parcel)?));
            }
        }
        Ok(None)
    }

    /// The first `u32` field with `id`, if any.
    pub fn u32_field(parcel: &Parcel, id: u16) -> Result<Option<u32>> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind == Kind::U32 && field.id == id {
                return Ok(Some(field.as_u32().map_err(Error::Parcel)?));
            }
        }
        Ok(None)
    }

    /// The first `bool` field with `id` (default `false`).
    pub fn bool_field(parcel: &Parcel, id: u16) -> Result<bool> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind == Kind::Bool && field.id == id {
                return field.as_bool().map_err(Error::Parcel);
            }
        }
        Ok(false)
    }

    /// The first `Bytes` field with `id`, if any.
    pub fn bytes_field(parcel: &Parcel, id: u16) -> Result<Option<Vec<u8>>> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind == Kind::Bytes && field.id == id {
                return Ok(Some(field.as_bytes().to_vec()));
            }
        }
        Ok(None)
    }

    /// Decode the first nested `EVENT` record, if the reply carries one.
    pub fn decode_event(parcel: &Parcel) -> Result<Option<Event>> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(record) = decoder.next().map_err(Error::Parcel)? {
            if record.kind != Kind::Struct || record.id != field::EVENT {
                continue;
            }
            let mut nested = record.nested(0).map_err(Error::Parcel)?;
            let mut event = Event {
                topic: String::new(),
                publisher: 0,
                sequence: 0,
                retained: false,
                payload: Vec::new(),
            };
            while let Some(item) = nested.next().map_err(Error::Parcel)? {
                match (item.kind, item.id) {
                    (Kind::String, field::TOPIC) => {
                        event.topic = String::from(item.as_str().map_err(Error::Parcel)?);
                    }
                    (Kind::U64, field::PUBLISHER) => {
                        event.publisher = item.as_u64().map_err(Error::Parcel)?;
                    }
                    (Kind::U64, field::SEQUENCE) => {
                        event.sequence = item.as_u64().map_err(Error::Parcel)?;
                    }
                    (Kind::Bool, field::RETAINED) => {
                        event.retained = item.as_bool().map_err(Error::Parcel)?;
                    }
                    (Kind::Bytes, field::PAYLOAD) => {
                        event.payload = item.as_bytes().to_vec();
                    }
                    _ => {}
                }
            }
            return Ok(Some(event));
        }
        Ok(None)
    }

    /// Decode a stats reply.
    pub fn decode_stats(parcel: &Parcel) -> Result<SubscriptionStats> {
        Ok(SubscriptionStats {
            qos: u32_field(parcel, field::QOS)?.unwrap_or(0),
            depth: u32_field(parcel, field::DEPTH)?.unwrap_or(0),
            queued: u64_field(parcel, field::QUEUED)?.unwrap_or(0),
            delivered: u64_field(parcel, field::DELIVERED)?.unwrap_or(0),
            matched: u64_field(parcel, field::MATCHED)?.unwrap_or(0),
            drops: u64_field(parcel, field::DROPS)?.unwrap_or(0),
        })
    }

    /// Decode a topic-list reply.
    pub fn decode_topics(parcel: &Parcel) -> Result<Vec<TopicInfo>> {
        let mut topics = Vec::new();
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(record) = decoder.next().map_err(Error::Parcel)? {
            if record.kind != Kind::Struct || record.id != field::ENTRY {
                continue;
            }
            let mut nested = record.nested(0).map_err(Error::Parcel)?;
            let mut info = TopicInfo {
                topic: String::new(),
                subscribers: 0,
                retained: false,
            };
            while let Some(item) = nested.next().map_err(Error::Parcel)? {
                match (item.kind, item.id) {
                    (Kind::String, field::TOPIC) => {
                        info.topic = String::from(item.as_str().map_err(Error::Parcel)?);
                    }
                    (Kind::U64, field::SUBSCRIBERS) => {
                        info.subscribers = item.as_u64().map_err(Error::Parcel)?;
                    }
                    (Kind::Bool, field::RETAINED) => {
                        info.retained = item.as_bool().map_err(Error::Parcel)?;
                    }
                    _ => {}
                }
            }
            topics.push(info);
        }
        Ok(topics)
    }

    /// Encode a `Publish` request body.
    fn publish_body(topic: &str, payload: &[u8], retained: bool) -> Result<Encoder> {
        let mut body = Encoder::new();
        body.string(field::TOPIC, topic).map_err(Error::Parcel)?;
        body.bytes(field::PAYLOAD, payload).map_err(Error::Parcel)?;
        body.bool(field::RETAINED, retained)
            .map_err(Error::Parcel)?;
        Ok(body)
    }

    /// Encode a `Subscribe` request body.
    fn subscribe_body(filter: &str, qos: Qos) -> Result<Encoder> {
        let mut body = Encoder::new();
        body.string(field::FILTER, filter).map_err(Error::Parcel)?;
        body.u32(field::QOS, qos.code()).map_err(Error::Parcel)?;
        body.u32(field::DEPTH, qos.depth()).map_err(Error::Parcel)?;
        Ok(body)
    }

    /// Encode a request body that names one subscription.
    fn subscription_body(id: u64) -> Result<Encoder> {
        let mut body = Encoder::new();
        body.u64(field::SUBSCRIPTION, id).map_err(Error::Parcel)?;
        Ok(body)
    }

    /// A client of the topics broker over the bootstrap channel.
    pub struct Client {
        endpoint: Endpoint,
    }

    impl Client {
        /// Resolve [`NAME`] and wrap the broker endpoint. Retries briefly
        /// while `messengerd` is still registering the name at boot.
        pub fn connect() -> Result<Client> {
            let first = match registry::resolve(NAME) {
                Ok(endpoint) => return Ok(Client { endpoint }),
                Err(error) => error,
            };
            if first.errno() != Some(-errno::ENOENT) {
                return Err(first);
            }
            // Park one tick per retry on a private pair; `close` frees the
            // pair when its last side goes (no channel is leaked). The probe
            // recv reuses one stack buffer because the bump heap never frees.
            let (probe, peer) = super::create_pair()?;
            let mut scratch = [0u8; 64];
            let mut client = Err(Error::Errno(-errno::ENOENT));
            for _ in 0..CONNECT_ATTEMPTS {
                let _ = probe.recv_into(&mut scratch, Some(EXPIRED_DEADLINE));
                match registry::resolve(NAME) {
                    Ok(endpoint) => {
                        client = Ok(Client { endpoint });
                        break;
                    }
                    Err(error) if error.errno() == Some(-errno::ENOENT) => {}
                    Err(error) => {
                        client = Err(error);
                        break;
                    }
                }
            }
            let _ = probe.close();
            let _ = peer.close();
            client
        }

        /// Wrap an already-resolved broker endpoint.
        pub fn from_endpoint(endpoint: Endpoint) -> Client {
            Client { endpoint }
        }

        /// The underlying broker endpoint (diagnostics).
        pub fn endpoint(&self) -> Endpoint {
            self.endpoint
        }

        /// Run one request as a blocking call and fail on a broker error reply.
        fn call(&self, method: u32, body: Encoder, deadline: Option<u64>) -> Result<Parcel> {
            let reply = self
                .endpoint
                .call(&request_parcel(method, body), deadline)?;
            if let Some(code) = error_field(&reply)? {
                return Err(Error::Topics(code));
            }
            Ok(reply)
        }

        /// Publish an opaque payload parcel under `topic`; returns how many
        /// subscriptions matched.
        pub fn publish(&self, topic: &str, payload: &Parcel) -> Result<u64> {
            self.publish_inner(topic, payload, false)
        }

        /// Publish `payload` and remember it as the topic's retained value
        /// (`docs/messenger.md` section 7.2).
        pub fn publish_retained(&self, topic: &str, payload: &Parcel) -> Result<u64> {
            self.publish_inner(topic, payload, true)
        }

        fn publish_inner(&self, topic: &str, payload: &Parcel, retained: bool) -> Result<u64> {
            let bytes = encode(payload)?;
            if bytes.len() > MAX_PAYLOAD {
                return Err(Error::Errno(-errno::E2BIG));
            }
            let reply = self.call(
                method::PUBLISH,
                publish_body(topic, &bytes, retained)?,
                None,
            )?;
            Ok(u64_field(&reply, field::MATCHED)?.unwrap_or(0))
        }

        /// Subscribe to `filter` (literal, `+` or trailing `#`) with `qos`.
        pub fn subscribe(&self, filter: &str, qos: Qos) -> Result<Subscription> {
            let reply = self.call(method::SUBSCRIBE, subscribe_body(filter, qos)?, None)?;
            let id = u64_field(&reply, field::SUBSCRIPTION)?.ok_or(Error::Errno(-errno::EINVAL))?;
            Ok(Subscription {
                endpoint: self.endpoint,
                id,
            })
        }

        /// Drop a subscription (same as [`Subscription::unsubscribe`]).
        pub fn unsubscribe(&self, subscription: &Subscription) -> Result<()> {
            self.call(
                method::UNSUBSCRIBE,
                subscription_body(subscription.id)?,
                None,
            )?;
            Ok(())
        }

        /// List topics the broker has seen, with live subscriber counts.
        pub fn list(&self) -> Result<Vec<TopicInfo>> {
            let reply = self.call(method::LIST_TOPICS, Encoder::new(), None)?;
            decode_topics(&reply)
        }

        /// Per-subscription counters (drops, queue depth, delivery).
        pub fn stats(&self, subscription: &Subscription) -> Result<SubscriptionStats> {
            let reply = self.call(method::STATS, subscription_body(subscription.id)?, None)?;
            decode_stats(&reply)
        }

        /// Round-trip probe.
        pub fn ping(&self) -> Result<()> {
            self.call(method::PING, Encoder::new(), None)?;
            Ok(())
        }
    }

    /// A live subscription on the broker.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Subscription {
        endpoint: Endpoint,
        id: u64,
    }

    impl Subscription {
        /// The broker-side subscription id.
        pub const fn id(self) -> u64 {
            self.id
        }

        /// Wait for the next event; `Ok(None)` means the deadline passed.
        ///
        /// With `reliable` QoS the broker redelivers the head until it is
        /// acked, so the same event can come back more than once; call
        /// [`Subscription::ack`] once the payload is safely processed.
        pub fn next_event(&self, deadline: Option<u64>) -> Result<Option<Event>> {
            let reply = match self.endpoint.call(
                &request_parcel(method::NEXT_EVENT, subscription_body(self.id)?),
                deadline,
            ) {
                Ok(reply) => reply,
                // The kernel deadline is the timeout signal; the broker simply
                // discovers a dead pull when it later tries to answer it.
                Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => return Ok(None),
                Err(error) => return Err(error),
            };
            if let Some(code) = error_field(&reply)? {
                return Err(Error::Topics(code));
            }
            decode_event(&reply)
        }

        /// [`Subscription::next_event`] with an already-expired deadline: never
        /// blocks, `Ok(None)` when nothing is queued.
        pub fn poll_event(&self) -> Result<Option<Event>> {
            self.next_event(Some(EXPIRED_DEADLINE))
        }

        /// Retire every event up to `sequence` (drives `reliable` queues).
        pub fn ack(&self, sequence: u64) -> Result<()> {
            let mut body = subscription_body(self.id)?;
            body.u64(field::SEQUENCE, sequence).map_err(Error::Parcel)?;
            let reply = self
                .endpoint
                .call(&request_parcel(method::ACK, body), None)?;
            if let Some(code) = error_field(&reply)? {
                return Err(Error::Topics(code));
            }
            Ok(())
        }

        /// Per-subscription counters.
        pub fn stats(&self) -> Result<SubscriptionStats> {
            let reply = self.endpoint.call(
                &request_parcel(method::STATS, subscription_body(self.id)?),
                None,
            )?;
            if let Some(code) = error_field(&reply)? {
                return Err(Error::Topics(code));
            }
            decode_stats(&reply)
        }

        /// Drop this subscription; later publishes stop matching it.
        pub fn unsubscribe(self) -> Result<()> {
            let reply = self.endpoint.call(
                &request_parcel(method::UNSUBSCRIBE, subscription_body(self.id)?),
                None,
            )?;
            if let Some(code) = error_field(&reply)? {
                return Err(Error::Topics(code));
            }
            Ok(())
        }
    }

    /// Ask the kernel policy engine about `name` for the task in `actor`
    /// (the `messengerd` proxy path). `mode` is [`MODE_PUBLISH`] or
    /// [`MODE_SUBSCRIBE`]; `txn` is copied into denial audit records.
    ///
    /// `-EACCES` means policy refused a segment; the denial is already in the
    /// audit ring.
    pub fn authorize(actor: u64, mode: u32, name: &str, txn: u64) -> Result<()> {
        let mut body = Encoder::new();
        body.string(auth_field::NAME, name).map_err(Error::Parcel)?;
        body.u32(auth_field::MODE, mode).map_err(Error::Parcel)?;
        body.u64(auth_field::TXN, txn).map_err(Error::Parcel)?;
        // The kernel op ignores the parcel header; the body carries the query.
        let parcel = request_parcel(0, body);
        let bytes = encode(&parcel)?;
        let args = MsgArgs {
            txn_id: actor,
            parcel_ptr: bytes.as_ptr() as u64,
            parcel_len: bytes.len() as u64,
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        syscall(op::AUTHORIZE_TOPIC, &args, &mut result)?;
        Ok(())
    }

    /// Convenience: connect and publish. Reusing a [`Client`] is cheaper, but
    /// this keeps one-shot callers short.
    pub fn publish(topic: &str, payload: &Parcel) -> Result<u64> {
        Client::connect()?.publish(topic, payload)
    }

    /// Convenience: connect and publish a retained value.
    pub fn publish_retained(topic: &str, payload: &Parcel) -> Result<u64> {
        Client::connect()?.publish_retained(topic, payload)
    }

    /// Convenience: connect and subscribe.
    pub fn subscribe(filter: &str, qos: Qos) -> Result<Subscription> {
        Client::connect()?.subscribe(filter, qos)
    }
}

/// Read `Stats` into an aligned local and hand the kernel its address. The
/// kernel writes little-endian `u64`s in the same field order, so on x86_64 the
/// struct is already the wire layout.
fn stats_call(handle: u64) -> Result<Stats> {
    let mut stats = Stats::default();
    let args = MsgArgs {
        handle,
        buf_ptr: &mut stats as *mut Stats as u64,
        buf_cap: Stats::SIZE as u64,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    syscall(op::STATS, &args, &mut result)?;
    if result.bytes as usize != Stats::SIZE {
        return Err(Error::Errno(-errno::E2BIG));
    }
    Ok(stats)
}

// ---------------------------------------------------------------------------
// Topics: the userspace event router (issue #93)
// ---------------------------------------------------------------------------

/// The userspace topic router the S2 services share (issue #93).
///
/// `docs/messenger.md` section 7 puts topics in `messengerd`, moved by the
/// kernel; that broker is still being built in `kernel/src/ipc` and the native
/// receive op does not yet surface transferred handles. Until it lands, the
/// supervisor services run this **interim router** over what the fabric does
/// support today:
///
/// * a service embeds a [`TopicBroker`] on its endpoint and registers the
///   endpoint's interface id in the kernel name registry;
/// * a subscriber calls [`Bus::subscribe`]; the broker hands it a unique sink
///   name (a counter with its prefix), the subscriber registers one end of its
///   own channel pair under that name with [`crate::messenger::registry`], and
///   the broker resolves the name (the kernel opens the endpoint straight into
///   the broker's table) and pushes [`Event`] parcels to it;
/// * topics are hierarchical with the spec's wildcards: `+` matches one
///   segment, `#` zero or more trailing segments;
/// * a broker retains the latest message per topic ([`TopicBroker::publish`]'s
///   `retained` flag) and replays matching retained values to a new subscriber,
///   which is what lets `logd` see service events that predate its start.
///
/// Service events use `system/events/<...>` (`system/events/service/<name>`
/// carries a service's state) and health state uses `system/health/<service>`,
/// as the platform plan names them. Only the transport changes when the
/// kernel/`messengerd` topic path lands; the topic names and payloads stay.
pub mod router {
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

    use super::{create_pair, errno, registry, Endpoint, Error, Message, Result};

    /// Topic router interface id. The human interface is
    /// `os.lazy.local.topics.v1`; this is its interim eight-byte ABI id (a
    /// `midlc` hash replaces it when the idl compiler owns the surface).
    pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.topic");

    /// Router methods.
    pub mod method {
        /// Allocate a unique sink name for a would-be subscriber.
        pub const RESERVE: u32 = 1;
        /// Attach a registered sink endpoint with a topic filter.
        pub const SUBSCRIBE: u32 = 2;
        /// Detach a sink endpoint.
        pub const UNSUBSCRIBE: u32 = 3;
        /// Publish a message on a topic (optionally retained).
        pub const PUBLISH: u32 = 4;
        /// Broker -> subscriber event delivery (one-way).
        pub const EVENT: u32 = 5;
    }

    /// Router TLV field ids.
    pub mod field {
        pub const FILTER: u16 = 1;
        pub const SINK: u16 = 2;
        pub const TOPIC: u16 = 3;
        pub const PAYLOAD: u16 = 4;
        pub const RETAINED: u16 = 5;
        pub const SEQ: u16 = 6;
    }

    /// Most recent retained messages a broker keeps (oldest dropped first).
    pub const RETAIN_LIMIT: usize = 64;

    /// A header for a topic-router parcel of `method`.
    fn header(method: u32) -> Header {
        Header {
            version: VERSION,
            flags: 0,
            interface_id: INTERFACE,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        }
    }

    /// Wrap an encoded body in a topic-router parcel.
    pub fn parcel(method: u32, body: Encoder) -> Parcel {
        Parcel {
            header: header(method),
            body: body.finish(),
            handles: Vec::new(),
            buffers: Vec::new(),
        }
    }

    /// Whether `topic` matches subscription `filter`: `+` matches exactly one
    /// segment, `#` matches zero or more trailing segments.
    pub fn matches(filter: &str, topic: &str) -> bool {
        let mut filter_segments = filter.split('/');
        let mut topic_segments = topic.split('/');
        loop {
            match (filter_segments.next(), topic_segments.next()) {
                (Some("#"), _) => return true,
                (Some("+"), Some(_)) => {}
                (Some(expected), Some(actual)) if expected == actual => {}
                (None, None) => return true,
                _ => return false,
            }
        }
    }

    /// A message a broker retains and replays to new subscribers.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Retained {
        /// Topic the message was published on.
        pub topic: String,
        /// Opaque publisher payload.
        pub payload: Vec<u8>,
        /// Publish sequence assigned by the broker.
        pub seq: u64,
    }

    /// One event delivered to a subscriber.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Event {
        /// Topic the message was published on.
        pub topic: String,
        /// Opaque publisher payload.
        pub payload: Vec<u8>,
        /// Whether the event was retained by the broker.
        pub retained: bool,
        /// Broker publish sequence.
        pub seq: u64,
    }

    impl Event {
        /// Decode an `EVENT` parcel; other methods/interfaces are `EINVAL`.
        pub fn from_message(message: &Message) -> Result<Event> {
            if message.interface_id() != INTERFACE || message.method() != method::EVENT {
                return Err(Error::Errno(-errno::EINVAL));
            }
            Ok(Event {
                topic: string_field(&message.parcel, field::TOPIC)?,
                payload: bytes_field(&message.parcel, field::PAYLOAD),
                retained: u64_field(&message.parcel, field::RETAINED).unwrap_or(0) != 0,
                seq: u64_field(&message.parcel, field::SEQ).unwrap_or(0),
            })
        }
    }

    /// One attached subscriber.
    struct Subscription {
        filter: String,
        sink: String,
        /// Cached endpoint; `None` until the sink resolves, or after a delivery
        /// failed (the next publish retries the registry).
        endpoint: Option<Endpoint>,
    }

    /// The broker half of the router: embed one in a service endpoint and call
    /// [`TopicBroker::handle`] for every message on the router interface.
    pub struct TopicBroker {
        prefix: &'static str,
        subscribers: Vec<Subscription>,
        retained: Vec<Retained>,
        next_sink: u64,
        next_seq: u64,
        /// Messages a publisher handed to the broker.
        pub published: u64,
        /// Events successfully pushed to subscribers.
        pub delivered: u64,
        /// Events refused because a subscriber was gone or its queue full.
        pub dropped: u64,
    }

    impl TopicBroker {
        /// A broker whose sink names are `<prefix>.<n>` (`prefix` must be a
        /// valid registry name segment; services use their own name).
        pub fn new(prefix: &'static str) -> TopicBroker {
            TopicBroker {
                prefix,
                subscribers: Vec::new(),
                retained: Vec::new(),
                next_sink: 0,
                next_seq: 0,
                published: 0,
                delivered: 0,
                dropped: 0,
            }
        }

        /// Serve one router request; the caller replies with the returned
        /// parcel when the message carried a transaction.
        pub fn handle(&mut self, message: &Message) -> Result<Parcel> {
            if message.interface_id() != INTERFACE {
                return Err(Error::Errno(-errno::EINVAL));
            }
            match message.method() {
                method::RESERVE => {
                    self.next_sink += 1;
                    let sink = format!("{}.{}", self.prefix, self.next_sink);
                    let mut body = Encoder::new();
                    body.string(field::SINK, &sink).map_err(Error::Parcel)?;
                    Ok(parcel(method::RESERVE, body))
                }
                method::SUBSCRIBE => {
                    let sink = string_field(&message.parcel, field::SINK)?;
                    let filter = string_field(&message.parcel, field::FILTER)?;
                    self.attach(filter, sink)?;
                    Ok(parcel(method::SUBSCRIBE, Encoder::new()))
                }
                method::UNSUBSCRIBE => {
                    let sink = string_field(&message.parcel, field::SINK)?;
                    self.subscribers
                        .retain(|subscriber| subscriber.sink != sink);
                    Ok(parcel(method::UNSUBSCRIBE, Encoder::new()))
                }
                method::PUBLISH => {
                    let topic = string_field(&message.parcel, field::TOPIC)?;
                    let payload = bytes_field(&message.parcel, field::PAYLOAD);
                    let retained = u64_field(&message.parcel, field::RETAINED).unwrap_or(0) != 0;
                    self.publish(&topic, &payload, retained);
                    Ok(parcel(method::PUBLISH, Encoder::new()))
                }
                _ => Err(Error::Errno(-errno::EINVAL)),
            }
        }

        /// Publish `payload` on `topic`, fanning out to matching subscribers;
        /// `retained` also keeps it for subscribers that arrive later.
        /// Returns the broker sequence number.
        pub fn publish(&mut self, topic: &str, payload: &[u8], retained: bool) -> u64 {
            self.next_seq = self.next_seq.wrapping_add(1);
            let seq = self.next_seq;
            self.published += 1;
            if retained {
                let entry = Retained {
                    topic: topic.to_string(),
                    payload: payload.to_vec(),
                    seq,
                };
                match self.retained.iter_mut().find(|entry| entry.topic == topic) {
                    Some(existing) => *existing = entry,
                    None => {
                        if self.retained.len() >= RETAIN_LIMIT {
                            self.retained.remove(0);
                        }
                        self.retained.push(entry);
                    }
                }
            }
            for index in 0..self.subscribers.len() {
                if !matches(&self.subscribers[index].filter, topic) {
                    continue;
                }
                let Ok(event) = event_parcel(topic, payload, retained, seq) else {
                    self.dropped += 1;
                    continue;
                };
                self.deliver(index, &event);
            }
            seq
        }

        /// The retained values, oldest first (introspection and tests).
        pub fn retained(&self) -> &[Retained] {
            &self.retained
        }

        /// Attached subscribers (introspection and tests).
        pub fn subscriber_count(&self) -> usize {
            self.subscribers.len()
        }

        /// Resolve `sink`, record the subscription, and replay the retained
        /// values `filter` already matches.
        fn attach(&mut self, filter: String, sink: String) -> Result<()> {
            if self
                .subscribers
                .iter()
                .any(|subscriber| subscriber.sink == sink)
            {
                return Ok(());
            }
            let endpoint = registry::resolve(&sink)?;
            let index = self.subscribers.len();
            self.subscribers.push(Subscription {
                filter,
                sink,
                endpoint: Some(endpoint),
            });
            let retained: Vec<(String, Vec<u8>, u64)> = self
                .retained
                .iter()
                .filter(|entry| matches(&self.subscribers[index].filter, &entry.topic))
                .map(|entry| (entry.topic.clone(), entry.payload.clone(), entry.seq))
                .collect();
            for (topic, payload, seq) in retained {
                if let Ok(event) = event_parcel(&topic, &payload, true, seq) {
                    self.deliver(index, &event);
                }
            }
            Ok(())
        }

        /// Push one event to subscriber `index`, falling back to one registry
        /// re-resolution when the cached endpoint is stale.
        fn deliver(&mut self, index: usize, event: &Parcel) {
            if let Some(endpoint) = self.subscribers[index].endpoint {
                if endpoint.send(event).is_ok() {
                    self.delivered += 1;
                    return;
                }
            }
            let resolved = registry::resolve(&self.subscribers[index].sink)
                .and_then(|endpoint| endpoint.send(event).map(|()| endpoint));
            match resolved {
                Ok(endpoint) => {
                    self.subscribers[index].endpoint = Some(endpoint);
                    self.delivered += 1;
                }
                Err(_) => {
                    self.subscribers[index].endpoint = None;
                    self.dropped += 1;
                }
            }
        }
    }

    /// Build an `EVENT` parcel for delivery to subscribers.
    fn event_parcel(topic: &str, payload: &[u8], retained: bool, seq: u64) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::TOPIC, topic).map_err(Error::Parcel)?;
        body.bytes(field::PAYLOAD, payload).map_err(Error::Parcel)?;
        body.u64(field::RETAINED, retained as u64)
            .map_err(Error::Parcel)?;
        body.u64(field::SEQ, seq).map_err(Error::Parcel)?;
        Ok(parcel(method::EVENT, body))
    }

    /// The client half: connect to a service's broker, subscribe, publish.
    pub struct Bus {
        endpoint: Endpoint,
    }

    impl Bus {
        /// Resolve `name` (a broker service, e.g. `os.lazy.healthd`).
        pub fn connect(name: &str) -> Result<Bus> {
            Ok(Bus {
                endpoint: registry::resolve(name)?,
            })
        }

        /// The underlying broker endpoint (diagnostics).
        pub fn endpoint(&self) -> Endpoint {
            self.endpoint
        }

        /// Subscribe to `filter`; returns a receiver already attached to the
        /// broker. Retained matching values are queued by the broker.
        pub fn subscribe(&self, filter: &str) -> Result<Subscriber> {
            // One round trip reserves a unique sink name; the subscriber then
            // registers its own endpoint under it, so no handle transfer is
            // needed across processes.
            let reply = self
                .endpoint
                .call(&parcel(method::RESERVE, Encoder::new()), None)?;
            let sink = string_field(&reply, field::SINK)?;
            let (published, received) = create_pair()?;
            registry::register(&sink, &published, &[], 0)?;

            let mut body = Encoder::new();
            body.string(field::SINK, &sink).map_err(Error::Parcel)?;
            body.string(field::FILTER, filter).map_err(Error::Parcel)?;
            self.endpoint.call(&parcel(method::SUBSCRIBE, body), None)?;
            Ok(Subscriber {
                endpoint: received,
                filter: String::from(filter),
            })
        }

        /// Publish `payload` on `topic` through the broker.
        pub fn publish(&self, topic: &str, payload: &[u8], retained: bool) -> Result<()> {
            let mut body = Encoder::new();
            body.string(field::TOPIC, topic).map_err(Error::Parcel)?;
            body.bytes(field::PAYLOAD, payload).map_err(Error::Parcel)?;
            body.u64(field::RETAINED, retained as u64)
                .map_err(Error::Parcel)?;
            self.endpoint.call(&parcel(method::PUBLISH, body), None)?;
            Ok(())
        }
    }

    /// A subscriber's receiving end.
    pub struct Subscriber {
        endpoint: Endpoint,
        /// The filter this subscriber attached with.
        pub filter: String,
    }

    impl Subscriber {
        /// Receive the next event, or `None` when `deadline` passes first.
        ///
        /// Allocates the receive buffer per call; a polling loop should use
        /// [`Subscriber::recv_with`] and reuse one buffer.
        pub fn recv(&self, deadline: Option<u64>) -> Result<Option<Event>> {
            let mut buf = alloc::vec![0u8; super::DEFAULT_BUFFER];
            self.recv_with(&mut buf, deadline)
        }

        /// [`Subscriber::recv`] with a caller-owned buffer.
        pub fn recv_with(&self, buf: &mut [u8], deadline: Option<u64>) -> Result<Option<Event>> {
            match self.endpoint.recv_with(buf, deadline) {
                Ok(message) => Ok(Some(Event::from_message(&message)?)),
                Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => Ok(None),
                Err(error) => Err(error),
            }
        }
    }

    /// The first string field with `id`.
    fn string_field(parcel: &Parcel, id: u16) -> Result<String> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind == Kind::String && field.id == id {
                return Ok(String::from(field.as_str().map_err(Error::Parcel)?));
            }
        }
        Err(Error::Errno(-errno::EINVAL))
    }

    /// The first `u64` field with `id`, if any.
    fn u64_field(parcel: &Parcel, id: u16) -> Option<u64> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Ok(Some(field)) = decoder.next() {
            if field.kind == Kind::U64 && field.id == id {
                return field.as_u64().ok();
            }
        }
        None
    }

    /// The first `bytes` field with `id` (`Vec::new` when absent).
    fn bytes_field(parcel: &Parcel, id: u16) -> Vec<u8> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Ok(Some(field)) = decoder.next() {
            if field.kind == Kind::Bytes && field.id == id {
                return field.as_bytes().to_vec();
            }
        }
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// System service interfaces: init, healthd, logd (issue #93)
// ---------------------------------------------------------------------------

/// Wire shapes of the S2 system services (`init`, `healthd`, `logd`), shared by
/// the services themselves and by `messengerctl`.
///
/// The topic names are the platform plan's:
///
/// * `system/events/service/<name>` â€” a service's state changed (payload:
///   `state=... pid=... status=... restarts=...`);
/// * `system/events/security/denial` â€” the audit counters advanced (the
///   interim signal until the kernel exposes audit records to userspace);
/// * `system/health/<name>` â€” retained health row published by `healthd`;
/// * `system/health/summary` â€” retained aggregate (worst status wins).
pub mod services {
    use alloc::string::String;
    use alloc::vec::Vec;

    use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

    use super::{errno, registry, Endpoint, Error, Result};

    /// The `init` supervisor's registered name.
    pub const INIT_NAME: &str = "os.lazy.init";
    /// The health aggregator's registered name.
    pub const HEALTHD_NAME: &str = "os.lazy.healthd";
    /// The structured event log's registered name.
    pub const LOGD_NAME: &str = "os.lazy.logd";

    /// `os.lazy.init.v1` (interim eight-byte ABI id, see [`super::topics`]).
    pub const INIT_INTERFACE: u64 = u64::from_le_bytes(*b"os.init.");
    /// `os.lazy.healthd.v1` (interim eight-byte ABI id).
    pub const HEALTHD_INTERFACE: u64 = u64::from_le_bytes(*b"os.healt");
    /// `os.lazy.logd.v1` (interim eight-byte ABI id).
    pub const LOGD_INTERFACE: u64 = u64::from_le_bytes(*b"os.logd.");

    /// `init` methods.
    pub mod init_method {
        /// Snapshot the supervision table.
        pub const SERVICES: u32 = 1;
    }

    /// `healthd` methods.
    pub mod healthd_method {
        /// Publish `health/<name>` with status/detail.
        pub const REPORT: u32 = 1;
        /// Snapshot retained health rows plus the summary.
        pub const STATUS: u32 = 2;
    }

    /// `logd` methods.
    pub mod logd_method {
        /// Return the newest `COUNT` records.
        pub const TAIL: u32 = 1;
        /// Return the number of records in the ring.
        pub const COUNT: u32 = 2;
        /// Recompute the hash chain and report `OK`/first bad `INDEX`.
        pub const VERIFY: u32 = 3;
    }

    /// Shared TLV field ids.
    pub mod field {
        /// Service name.
        pub const NAME: u16 = 1;
        /// Service phase (`pending`/`running`/`restarting`/`stopped`/`failed`).
        pub const STATE: u16 = 2;
        /// Task slot the supervisor started, or 0.
        pub const PID: u16 = 3;
        /// Restart count.
        pub const RESTARTS: u16 = 4;
        /// Comma-separated dependency names.
        pub const DEPS: u16 = 5;
        /// One record (service status or health row).
        pub const SERVICE: u16 = 6;
        /// Last known health string for a service.
        pub const HEALTH: u16 = 7;
        /// Health status (`ok`/`degraded`/`down`).
        pub const STATUS: u16 = 8;
        /// Human-readable detail.
        pub const DETAIL: u16 = 9;
        /// Tick the row/record was produced.
        pub const TICK: u16 = 10;
        /// One log record.
        pub const RECORD: u16 = 11;
        /// Log sequence number.
        pub const SEQ: u16 = 12;
        /// Log topic.
        pub const TOPIC: u16 = 13;
        /// Log chain hash.
        pub const HASH: u16 = 14;
        /// Requested/returned count.
        pub const COUNT: u16 = 15;
        /// Verify verdict (1 = chain intact).
        pub const OK: u16 = 16;
        /// First mismatching log index on a broken chain.
        pub const INDEX: u16 = 17;
        /// Aggregate health row.
        pub const SUMMARY: u16 = 18;
    }

    /// A header for a service parcel of `method` on `interface_id`.
    fn header(interface_id: u64, method: u32) -> Header {
        Header {
            version: VERSION,
            flags: 0,
            interface_id,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        }
    }

    /// One row of `init`'s supervision table.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct ServiceStatus {
        pub name: String,
        pub state: String,
        pub pid: u64,
        pub restarts: u64,
        pub deps: String,
        pub health: String,
    }

    /// One retained health row (`healthd`).
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct HealthRecord {
        pub name: String,
        pub status: String,
        pub detail: String,
        pub tick: u64,
    }

    /// One `logd` record (the hash chains over the previous record's hash).
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct LogRecord {
        pub seq: u64,
        pub tick: u64,
        pub topic: String,
        pub detail: String,
        pub hash: u64,
    }

    /// `init`'s `Services` request.
    pub fn services_request() -> Parcel {
        Parcel {
            header: header(INIT_INTERFACE, init_method::SERVICES),
            ..Parcel::default()
        }
    }

    /// Encode `init`'s `Services` reply: one `SERVICE` record per row.
    pub fn services_reply(statuses: &[ServiceStatus]) -> Result<Parcel> {
        let mut body = Encoder::new();
        for status in statuses {
            let mut record = Encoder::new();
            record
                .string(field::NAME, &status.name)
                .map_err(Error::Parcel)?;
            record
                .string(field::STATE, &status.state)
                .map_err(Error::Parcel)?;
            record.u64(field::PID, status.pid).map_err(Error::Parcel)?;
            record
                .u64(field::RESTARTS, status.restarts)
                .map_err(Error::Parcel)?;
            record
                .string(field::DEPS, &status.deps)
                .map_err(Error::Parcel)?;
            record
                .string(field::HEALTH, &status.health)
                .map_err(Error::Parcel)?;
            body.record(field::SERVICE, &record)
                .map_err(Error::Parcel)?;
        }
        Ok(Parcel {
            header: header(INIT_INTERFACE, init_method::SERVICES),
            body: body.finish(),
            ..Parcel::default()
        })
    }

    /// `healthd`'s `Status` request.
    pub fn health_request() -> Parcel {
        Parcel {
            header: header(HEALTHD_INTERFACE, healthd_method::STATUS),
            ..Parcel::default()
        }
    }

    /// `healthd`'s `Report` request: publish `health/<name>`.
    pub fn health_report_request(name: &str, status: &str, detail: &str) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::NAME, name).map_err(Error::Parcel)?;
        body.string(field::STATUS, status).map_err(Error::Parcel)?;
        body.string(field::DETAIL, detail).map_err(Error::Parcel)?;
        Ok(Parcel {
            header: header(HEALTHD_INTERFACE, healthd_method::REPORT),
            body: body.finish(),
            ..Parcel::default()
        })
    }

    /// Encode `healthd`'s `Status` reply: a `SUMMARY` record, then one
    /// `SERVICE` record per retained health row.
    pub fn health_reply(summary: &HealthRecord, records: &[HealthRecord]) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.record(field::SUMMARY, &health_record_encoder(summary)?)
            .map_err(Error::Parcel)?;
        for record in records {
            body.record(field::SERVICE, &health_record_encoder(record)?)
                .map_err(Error::Parcel)?;
        }
        Ok(Parcel {
            header: header(HEALTHD_INTERFACE, healthd_method::STATUS),
            body: body.finish(),
            ..Parcel::default()
        })
    }

    fn health_record_encoder(record: &HealthRecord) -> Result<Encoder> {
        let mut encoder = Encoder::new();
        encoder
            .string(field::NAME, &record.name)
            .map_err(Error::Parcel)?;
        encoder
            .string(field::STATUS, &record.status)
            .map_err(Error::Parcel)?;
        encoder
            .string(field::DETAIL, &record.detail)
            .map_err(Error::Parcel)?;
        encoder
            .u64(field::TICK, record.tick)
            .map_err(Error::Parcel)?;
        Ok(encoder)
    }

    /// `logd`'s `Tail` request.
    pub fn log_tail_request(count: u64) -> Parcel {
        let mut body = Encoder::new();
        // The request cannot fail: a fresh encoder has room for one field.
        let _ = body.u64(field::COUNT, count);
        Parcel {
            header: header(LOGD_INTERFACE, logd_method::TAIL),
            body: body.finish(),
            ..Parcel::default()
        }
    }

    /// `logd`'s `Count` request.
    pub fn log_count_request() -> Parcel {
        Parcel {
            header: header(LOGD_INTERFACE, logd_method::COUNT),
            ..Parcel::default()
        }
    }

    /// `logd`'s `Verify` request.
    pub fn log_verify_request() -> Parcel {
        Parcel {
            header: header(LOGD_INTERFACE, logd_method::VERIFY),
            ..Parcel::default()
        }
    }

    /// Encode `logd`'s `Tail` reply.
    pub fn log_records_reply(records: &[LogRecord]) -> Result<Parcel> {
        let mut body = Encoder::new();
        for record in records {
            let mut nested = Encoder::new();
            nested.u64(field::SEQ, record.seq).map_err(Error::Parcel)?;
            nested
                .u64(field::TICK, record.tick)
                .map_err(Error::Parcel)?;
            nested
                .string(field::TOPIC, &record.topic)
                .map_err(Error::Parcel)?;
            nested
                .string(field::DETAIL, &record.detail)
                .map_err(Error::Parcel)?;
            nested
                .u64(field::HASH, record.hash)
                .map_err(Error::Parcel)?;
            body.record(field::RECORD, &nested).map_err(Error::Parcel)?;
        }
        Ok(Parcel {
            header: header(LOGD_INTERFACE, logd_method::TAIL),
            body: body.finish(),
            ..Parcel::default()
        })
    }

    /// Encode `logd`'s `Count` reply.
    pub fn log_count_reply(count: u64) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::COUNT, count).map_err(Error::Parcel)?;
        Ok(Parcel {
            header: header(LOGD_INTERFACE, logd_method::COUNT),
            body: body.finish(),
            ..Parcel::default()
        })
    }

    /// Encode `logd`'s `Verify` reply: `OK` (1/0) and the first bad `INDEX`
    /// (the record count when the chain is intact).
    pub fn log_verify_reply(ok: bool, index: u64) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::OK, ok as u64).map_err(Error::Parcel)?;
        body.u64(field::INDEX, index).map_err(Error::Parcel)?;
        Ok(Parcel {
            header: header(LOGD_INTERFACE, logd_method::VERIFY),
            body: body.finish(),
            ..Parcel::default()
        })
    }

    /// Resolve a service's registered name.
    pub fn resolve_service(name: &str) -> Result<Endpoint> {
        registry::resolve(name)
    }

    /// Call `init`'s `Services`.
    ///
    /// Allocates the reply buffer per call; a polling loop should use
    /// [`fetch_services_with`] and reuse one buffer.
    pub fn fetch_services(endpoint: &Endpoint) -> Result<Vec<ServiceStatus>> {
        let mut buf = alloc::vec![0u8; super::DEFAULT_BUFFER];
        fetch_services_with(endpoint, &mut buf)
    }

    /// [`fetch_services`] with a caller-owned reply buffer.
    pub fn fetch_services_with(endpoint: &Endpoint, buf: &mut [u8]) -> Result<Vec<ServiceStatus>> {
        let reply = endpoint.call_with(&services_request(), buf, None)?;
        let mut statuses = Vec::new();
        for_each_record(&reply, |mut nested| {
            let mut status = ServiceStatus::default();
            while let Ok(Some(field)) = nested.next() {
                match (field.kind, field.id) {
                    (Kind::String, field::NAME) => {
                        status.name = String::from(field.as_str().map_err(Error::Parcel)?)
                    }
                    (Kind::String, field::STATE) => {
                        status.state = String::from(field.as_str().map_err(Error::Parcel)?)
                    }
                    (Kind::U64, field::PID) => {
                        status.pid = field.as_u64().map_err(Error::Parcel)?
                    }
                    (Kind::U64, field::RESTARTS) => {
                        status.restarts = field.as_u64().map_err(Error::Parcel)?
                    }
                    (Kind::String, field::DEPS) => {
                        status.deps = String::from(field.as_str().map_err(Error::Parcel)?)
                    }
                    (Kind::String, field::HEALTH) => {
                        status.health = String::from(field.as_str().map_err(Error::Parcel)?)
                    }
                    _ => {}
                }
            }
            statuses.push(status);
            Ok(())
        })?;
        Ok(statuses)
    }

    /// Call `healthd`'s `Status`; returns the summary and the retained rows.
    pub fn fetch_health(endpoint: &Endpoint) -> Result<(HealthRecord, Vec<HealthRecord>)> {
        let reply = endpoint.call(&health_request(), None)?;
        let mut summary = HealthRecord::default();
        let mut records = Vec::new();
        let mut decoder = Decoder::new(&reply.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind != Kind::Struct {
                continue;
            }
            let mut nested = field.nested(0).map_err(Error::Parcel)?;
            let mut record = HealthRecord::default();
            while let Some(item) = nested.next().map_err(Error::Parcel)? {
                match (item.kind, item.id) {
                    (Kind::String, field::NAME) => {
                        record.name = String::from(item.as_str().map_err(Error::Parcel)?)
                    }
                    (Kind::String, field::STATUS) => {
                        record.status = String::from(item.as_str().map_err(Error::Parcel)?)
                    }
                    (Kind::String, field::DETAIL) => {
                        record.detail = String::from(item.as_str().map_err(Error::Parcel)?)
                    }
                    (Kind::U64, field::TICK) => {
                        record.tick = item.as_u64().map_err(Error::Parcel)?
                    }
                    _ => {}
                }
            }
            match field.id {
                field::SUMMARY => summary = record,
                field::SERVICE => records.push(record),
                _ => {}
            }
        }
        Ok((summary, records))
    }

    /// Call `logd`'s `Tail`.
    pub fn fetch_log_tail(endpoint: &Endpoint, count: u64) -> Result<Vec<LogRecord>> {
        let reply = endpoint.call(&log_tail_request(count), None)?;
        decode_log_records(&reply)
    }

    /// Call `logd`'s `Count`.
    pub fn fetch_log_count(endpoint: &Endpoint) -> Result<u64> {
        let reply = endpoint.call(&log_count_request(), None)?;
        first_u64(&reply).ok_or(Error::Errno(-errno::EINVAL))
    }

    /// Call `logd`'s `Verify`; returns `(intact, first bad index)`.
    pub fn fetch_log_verify(endpoint: &Endpoint) -> Result<(bool, u64)> {
        let reply = endpoint.call(&log_verify_request(), None)?;
        let ok = first_u64(&reply).unwrap_or(0) != 0;
        let index = all_u64(&reply).nth(1).unwrap_or(0);
        Ok((ok, index))
    }

    /// Decode a `Tail` reply into records.
    pub fn decode_log_records(parcel: &Parcel) -> Result<Vec<LogRecord>> {
        let mut records = Vec::new();
        for_each_record(parcel, |mut nested| {
            let mut record = LogRecord::default();
            while let Ok(Some(field)) = nested.next() {
                match (field.kind, field.id) {
                    (Kind::U64, field::SEQ) => {
                        record.seq = field.as_u64().map_err(Error::Parcel)?
                    }
                    (Kind::U64, field::TICK) => {
                        record.tick = field.as_u64().map_err(Error::Parcel)?
                    }
                    (Kind::String, field::TOPIC) => {
                        record.topic = String::from(field.as_str().map_err(Error::Parcel)?)
                    }
                    (Kind::String, field::DETAIL) => {
                        record.detail = String::from(field.as_str().map_err(Error::Parcel)?)
                    }
                    (Kind::U64, field::HASH) => {
                        record.hash = field.as_u64().map_err(Error::Parcel)?
                    }
                    _ => {}
                }
            }
            records.push(record);
            Ok(())
        })?;
        Ok(records)
    }

    /// Iterate `SERVICE`/`RECORD` struct fields of a reply body.
    fn for_each_record(
        parcel: &Parcel,
        mut body: impl FnMut(Decoder<'_>) -> Result<()>,
    ) -> Result<()> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind != Kind::Struct {
                continue;
            }
            body(field.nested(0).map_err(Error::Parcel)?)?;
        }
        Ok(())
    }

    /// The first top-level `u64` field (regardless of id).
    fn first_u64(parcel: &Parcel) -> Option<u64> {
        all_u64(parcel).next()
    }

    /// Every top-level `u64` field.
    fn all_u64(parcel: &Parcel) -> impl Iterator<Item = u64> + '_ {
        let mut decoder = Decoder::new(&parcel.body);
        core::iter::from_fn(move || {
            while let Ok(Some(field)) = decoder.next() {
                if field.kind == Kind::U64 {
                    if let Ok(value) = field.as_u64() {
                        return Some(value);
                    }
                }
            }
            None
        })
    }
}

// ---------------------------------------------------------------------------
// Accounts, keyd, and login interfaces (issue #101)
// ---------------------------------------------------------------------------

/// `keyd` client shape: password verification happens in the key service, and
/// the verifier never leaves it (`docs/security-model.md` section 3, section 8).
///
/// `keyd` does not exist in this branch yet, so the client is written against
/// its documented interface: `Verify(NAME, SECRET)` answers `OK=1` when the
/// secret matches the user's stored verifier. `accountsd` probes for the
/// service and falls back to its bring-up verifier only when it is absent.
pub mod keyd {
    use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

    use super::{Endpoint, Error, Result};

    /// The key service's registered name.
    pub const NAME: &str = "os.lazy.keyd";

    /// `os.lazy.keyd.v1` as an interim eight-byte ABI id.
    pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.keyd.");

    /// Key-service methods.
    pub mod method {
        /// Verify a secret against a user's stored password verifier.
        pub const VERIFY: u32 = 1;
    }

    /// Key-service TLV field ids.
    pub mod field {
        /// Account name.
        pub const NAME: u16 = 1;
        /// Secret to verify (never logged).
        pub const SECRET: u16 = 2;
        /// Verification verdict (`1` = match).
        pub const OK: u16 = 3;
    }

    /// A `Verify` request parcel.
    pub fn verify_request(name: &str, secret: &str) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::NAME, name).map_err(Error::Parcel)?;
        body.string(field::SECRET, secret).map_err(Error::Parcel)?;
        Ok(Parcel {
            header: Header {
                version: VERSION,
                flags: 0,
                interface_id: INTERFACE,
                method: method::VERIFY,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body: body.finish(),
            ..Parcel::default()
        })
    }

    /// The first `u64` field with `id` in a `Verify` reply.
    fn u64_field(parcel: &Parcel, id: u16) -> Option<u64> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Ok(Some(field)) = decoder.next() {
            if field.kind == Kind::U64 && field.id == id {
                return field.as_u64().ok();
            }
        }
        None
    }

    /// Ask `keyd` to verify `secret` for `name`; `true` means the secret
    /// matched. An error means keyd could not answer (absent, malformed, or
    /// channel failure), which lets the caller fall back deliberately.
    pub fn verify(endpoint: &Endpoint, name: &str, secret: &str) -> Result<bool> {
        let reply = endpoint.call(&verify_request(name, secret)?, None)?;
        Ok(u64_field(&reply, field::OK).unwrap_or(0) != 0)
    }
}

/// `accountsd` client and server shapes (issue #101).
///
/// The account database answers three questions: who is a name or uid
/// (`Lookup`), is this secret theirs (`Authenticate`), and can an admin create
/// a new account (`CreateUser`). A record is the `/etc/passwd` shape minus the
/// verifier: name, uid, gid, home, shell. The verifier stays in the daemon's
/// private table (or keyd).
pub mod accounts {
    use alloc::string::String;

    use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

    use super::{errno, Endpoint, Error, Result};

    /// The accounts service's registered name.
    pub const NAME: &str = "os.lazy.accountsd";

    /// `os.lazy.accountsd.v1` as an interim eight-byte ABI id.
    pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.acct.");

    /// Accounts methods.
    pub mod method {
        /// Find a user by name or uid.
        pub const LOOKUP: u32 = 1;
        /// Verify a user's secret.
        pub const AUTHENTICATE: u32 = 2;
        /// Create a user (admin only).
        pub const CREATE: u32 = 3;
    }

    /// Accounts TLV field ids.
    pub mod field {
        /// Account name.
        pub const NAME: u16 = 1;
        /// Numeric user id (`0` = root).
        pub const UID: u16 = 2;
        /// Primary group id.
        pub const GID: u16 = 3;
        /// Secret to verify or the new user's initial secret.
        pub const SECRET: u16 = 4;
        /// Home directory.
        pub const HOME: u16 = 5;
        /// Login shell path.
        pub const SHELL: u16 = 6;
        /// Lookup verdict (`1` = the user exists).
        pub const FOUND: u16 = 7;
        /// Generic success verdict.
        pub const OK: u16 = 8;
        /// Human-readable detail for a refusal.
        pub const DETAIL: u16 = 9;
    }

    /// One account record, as a lookup reply carries it.
    #[derive(Clone, Default, PartialEq, Eq, Debug)]
    pub struct UserRecord {
        /// Account name.
        pub name: String,
        /// User id.
        pub uid: u32,
        /// Primary group id.
        pub gid: u32,
        /// Home directory.
        pub home: String,
        /// Login shell.
        pub shell: String,
    }

    /// A `CreateUser` request's full payload (the initial secret included).
    #[derive(Clone, Default, PartialEq, Eq, Debug)]
    pub struct NewUser {
        pub name: String,
        pub uid: u32,
        pub gid: u32,
        pub secret: String,
        pub home: String,
        pub shell: String,
    }

    /// A header for an accounts parcel of `method`.
    fn header(method: u32) -> Header {
        Header {
            version: VERSION,
            flags: 0,
            interface_id: INTERFACE,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        }
    }

    /// Wrap an encoded body in an accounts parcel.
    fn parcel(method: u32, body: Encoder) -> Parcel {
        Parcel {
            header: header(method),
            body: body.finish(),
            ..Parcel::default()
        }
    }

    /// A `Lookup` request by name.
    pub fn lookup_name_request(name: &str) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::NAME, name).map_err(Error::Parcel)?;
        Ok(parcel(method::LOOKUP, body))
    }

    /// A `Lookup` request by uid.
    pub fn lookup_uid_request(uid: u32) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::UID, uid as u64).map_err(Error::Parcel)?;
        Ok(parcel(method::LOOKUP, body))
    }

    /// An `Authenticate` request.
    pub fn authenticate_request(name: &str, secret: &str) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::NAME, name).map_err(Error::Parcel)?;
        body.string(field::SECRET, secret).map_err(Error::Parcel)?;
        Ok(parcel(method::AUTHENTICATE, body))
    }

    /// A `CreateUser` request (an admin's tool would send this).
    pub fn create_request(user: &NewUser) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::NAME, &user.name)
            .map_err(Error::Parcel)?;
        body.u64(field::UID, user.uid as u64)
            .map_err(Error::Parcel)?;
        body.u64(field::GID, user.gid as u64)
            .map_err(Error::Parcel)?;
        body.string(field::SECRET, &user.secret)
            .map_err(Error::Parcel)?;
        body.string(field::HOME, &user.home)
            .map_err(Error::Parcel)?;
        body.string(field::SHELL, &user.shell)
            .map_err(Error::Parcel)?;
        Ok(parcel(method::CREATE, body))
    }

    /// Encode a `Lookup` reply: `FOUND`, then the record when found.
    pub fn user_reply(user: Option<&UserRecord>) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::FOUND, user.is_some() as u64)
            .map_err(Error::Parcel)?;
        if let Some(user) = user {
            body.string(field::NAME, &user.name)
                .map_err(Error::Parcel)?;
            body.u64(field::UID, user.uid as u64)
                .map_err(Error::Parcel)?;
            body.u64(field::GID, user.gid as u64)
                .map_err(Error::Parcel)?;
            body.string(field::HOME, &user.home)
                .map_err(Error::Parcel)?;
            body.string(field::SHELL, &user.shell)
                .map_err(Error::Parcel)?;
        }
        Ok(parcel(method::LOOKUP, body))
    }

    /// Encode an `Authenticate` reply.
    pub fn auth_reply(matched: bool) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::OK, matched as u64).map_err(Error::Parcel)?;
        Ok(parcel(method::AUTHENTICATE, body))
    }

    /// Encode a `CreateUser` reply with the daemon's detail text.
    pub fn create_reply(ok: bool, detail: &str) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::OK, ok as u64).map_err(Error::Parcel)?;
        body.string(field::DETAIL, detail).map_err(Error::Parcel)?;
        Ok(parcel(method::CREATE, body))
    }

    /// The first string field with `id`.
    pub fn string_field(parcel: &Parcel, id: u16) -> Result<String> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind == Kind::String && field.id == id {
                return Ok(String::from(field.as_str().map_err(Error::Parcel)?));
            }
        }
        Err(Error::Errno(-errno::EINVAL))
    }

    /// The first string field with `id`, if any.
    pub fn optional_string(parcel: &Parcel, id: u16) -> Option<String> {
        string_field(parcel, id).ok()
    }

    /// The first `u64` field with `id`, if any.
    pub fn u64_field(parcel: &Parcel, id: u16) -> Option<u64> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Ok(Some(field)) = decoder.next() {
            if field.kind == Kind::U64 && field.id == id {
                return field.as_u64().ok();
            }
        }
        None
    }

    /// Decode a `Lookup` request into `(name, uid)`; exactly one is set.
    pub fn decode_lookup(parcel: &Parcel) -> Result<(Option<String>, Option<u64>)> {
        Ok((
            optional_string(parcel, field::NAME),
            u64_field(parcel, field::UID),
        ))
    }

    /// Decode an `Authenticate` request.
    pub fn decode_authenticate(parcel: &Parcel) -> Result<(String, String)> {
        Ok((
            string_field(parcel, field::NAME)?,
            string_field(parcel, field::SECRET)?,
        ))
    }

    /// Decode a `CreateUser` request.
    pub fn decode_create(parcel: &Parcel) -> Result<NewUser> {
        Ok(NewUser {
            name: string_field(parcel, field::NAME)?,
            uid: u64_field(parcel, field::UID).unwrap_or(0) as u32,
            gid: u64_field(parcel, field::GID).unwrap_or(0) as u32,
            secret: string_field(parcel, field::SECRET)?,
            home: optional_string(parcel, field::HOME).unwrap_or_default(),
            shell: optional_string(parcel, field::SHELL).unwrap_or_default(),
        })
    }

    /// Decode a `Lookup` reply into the record, or `None` when not found.
    pub fn decode_user(parcel: &Parcel) -> Result<Option<UserRecord>> {
        if u64_field(parcel, field::FOUND).unwrap_or(0) == 0 {
            return Ok(None);
        }
        Ok(Some(UserRecord {
            name: string_field(parcel, field::NAME)?,
            uid: u64_field(parcel, field::UID).unwrap_or(0) as u32,
            gid: u64_field(parcel, field::GID).unwrap_or(0) as u32,
            home: optional_string(parcel, field::HOME).unwrap_or_default(),
            shell: optional_string(parcel, field::SHELL).unwrap_or_default(),
        }))
    }

    /// Look a user up by name through the daemon.
    pub fn lookup_name(endpoint: &Endpoint, name: &str) -> Result<Option<UserRecord>> {
        let reply = endpoint.call(&lookup_name_request(name)?, None)?;
        decode_user(&reply)
    }

    /// Look a user up by uid through the daemon.
    pub fn lookup_uid(endpoint: &Endpoint, uid: u32) -> Result<Option<UserRecord>> {
        let reply = endpoint.call(&lookup_uid_request(uid)?, None)?;
        decode_user(&reply)
    }

    /// Ask the daemon whether `secret` belongs to `name`.
    pub fn authenticate(endpoint: &Endpoint, name: &str, secret: &str) -> Result<bool> {
        let reply = endpoint.call(&authenticate_request(name, secret)?, None)?;
        Ok(u64_field(&reply, field::OK).unwrap_or(0) != 0)
    }
}

/// `logind` client and server shapes (issue #101): the session table and the
/// query `messengerctl sessions` renders.
pub mod logind {
    use alloc::string::String;
    use alloc::vec::Vec;

    use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

    use super::{sys, Endpoint, Error, Result};

    /// The login service's registered name.
    pub const NAME: &str = "os.lazy.logind";

    /// `os.lazy.logind.v1` as an interim eight-byte ABI id.
    pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.login");

    /// `logind` methods.
    pub mod method {
        /// Snapshot the session table.
        pub const SESSIONS: u32 = 1;
    }

    /// `logind` TLV field ids.
    pub mod field {
        /// Session id.
        pub const ID: u16 = 2;
        /// Account name.
        pub const USER: u16 = 3;
        /// User id stamped on the session.
        pub const UID: u16 = 4;
        /// Task slot of the session's shell.
        pub const PID: u16 = 5;
        /// `active` or `exited`.
        pub const STATE: u16 = 6;
        /// Tick the session started.
        pub const STARTED: u16 = 7;
        /// Number of active sessions.
        pub const ACTIVE: u16 = 8;
        /// One session record.
        pub const SESSION: u16 = 9;
    }

    /// How long `fetch_sessions` waits for an answer (PIT ticks).
    ///
    /// `logind` can be sitting at the console prompt when a query arrives, in
    /// which case it answers after the next key; a bounded wait keeps a
    /// diagnostic tool from hanging forever.
    pub const QUERY_DEADLINE_TICKS: u64 = 100;

    /// One session row.
    #[derive(Clone, Default, PartialEq, Eq, Debug)]
    pub struct SessionRecord {
        /// Session id minted by `logind`.
        pub id: u64,
        /// Account name.
        pub user: String,
        /// User id.
        pub uid: u32,
        /// Task slot of the session's shell (`0` until spawned).
        pub pid: u64,
        /// `active` while the shell runs, `exited` after it is reaped.
        pub state: String,
        /// Tick the session started.
        pub started: u64,
    }

    /// A `Sessions` request parcel.
    pub fn sessions_request() -> Parcel {
        Parcel {
            header: Header {
                version: VERSION,
                flags: 0,
                interface_id: INTERFACE,
                method: method::SESSIONS,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            ..Parcel::default()
        }
    }

    /// Encode a `Sessions` reply: the active count, then one record per
    /// session, oldest first.
    pub fn sessions_reply(sessions: &[SessionRecord], active: u64) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::ACTIVE, active).map_err(Error::Parcel)?;
        for session in sessions {
            let mut record = Encoder::new();
            record.u64(field::ID, session.id).map_err(Error::Parcel)?;
            record
                .string(field::USER, &session.user)
                .map_err(Error::Parcel)?;
            record
                .u64(field::UID, session.uid as u64)
                .map_err(Error::Parcel)?;
            record.u64(field::PID, session.pid).map_err(Error::Parcel)?;
            record
                .string(field::STATE, &session.state)
                .map_err(Error::Parcel)?;
            record
                .u64(field::STARTED, session.started)
                .map_err(Error::Parcel)?;
            body.record(field::SESSION, &record)
                .map_err(Error::Parcel)?;
        }
        Ok(Parcel {
            header: Header {
                version: VERSION,
                flags: 0,
                interface_id: INTERFACE,
                method: method::SESSIONS,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body: body.finish(),
            ..Parcel::default()
        })
    }

    /// Decode a `Sessions` reply into `(active, records)`.
    pub fn decode_sessions(parcel: &Parcel) -> Result<(u64, Vec<SessionRecord>)> {
        let mut active = 0u64;
        let mut sessions = Vec::new();
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            match (field.kind, field.id) {
                (Kind::U64, field::ACTIVE) => {
                    active = field.as_u64().map_err(Error::Parcel)?;
                }
                (Kind::Struct, field::SESSION) => {
                    let mut nested = field.nested(0).map_err(Error::Parcel)?;
                    let mut session = SessionRecord::default();
                    while let Some(item) = nested.next().map_err(Error::Parcel)? {
                        match (item.kind, item.id) {
                            (Kind::U64, field::ID) => {
                                session.id = item.as_u64().map_err(Error::Parcel)?
                            }
                            (Kind::String, field::USER) => {
                                session.user = String::from(item.as_str().map_err(Error::Parcel)?)
                            }
                            (Kind::U64, field::UID) => {
                                session.uid = item.as_u64().map_err(Error::Parcel)? as u32
                            }
                            (Kind::U64, field::PID) => {
                                session.pid = item.as_u64().map_err(Error::Parcel)?
                            }
                            (Kind::String, field::STATE) => {
                                session.state = String::from(item.as_str().map_err(Error::Parcel)?)
                            }
                            (Kind::U64, field::STARTED) => {
                                session.started = item.as_u64().map_err(Error::Parcel)?
                            }
                            _ => {}
                        }
                    }
                    sessions.push(session);
                }
                _ => {}
            }
        }
        Ok((active, sessions))
    }

    /// Call `logind`'s `Sessions` with a bounded deadline.
    pub fn fetch_sessions(endpoint: &Endpoint) -> Result<(u64, Vec<SessionRecord>)> {
        let mut buf = alloc::vec![0u8; super::DEFAULT_BUFFER];
        let deadline = sys::clock().saturating_add(QUERY_DEADLINE_TICKS);
        let reply = endpoint.call_with(&sessions_request(), &mut buf, Some(deadline))?;
        if reply.header.method != method::SESSIONS {
            return Err(Error::Errno(-super::errno::EINVAL));
        }
        decode_sessions(&reply)
    }
}
