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
    pub const EBADMSG: i64 = 74;
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
    /// The MIME service refused the request with a positive errno-style code
    /// (`mimed` carries it in the reply's `ERROR` field).
    Mime(i64),
    /// The supervisor refused the request with a positive errno-style code
    /// (`init` carries it in the reply's `ERROR` field).
    Init(i64),
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
        self.call_bytes_with(&bytes, buf, deadline)
    }

    /// [`Endpoint::call_with`] for a caller that already holds the encoded
    /// request bytes, e.g. a poll loop that re-sends the same fixed request
    /// every call: `call_with` would otherwise re-encode (and reallocate) it
    /// every time, and the user runtime's bump allocator never reclaims that.
    pub fn call_bytes_with(
        &self,
        request: &[u8],
        buf: &mut [u8],
        deadline: Option<u64>,
    ) -> Result<Parcel> {
        let args = MsgArgs {
            handle: self.handle,
            parcel_ptr: request.as_ptr() as u64,
            parcel_len: request.len() as u64,
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
    /// it with [`Endpoint::await_reply`] Ã¢â‚¬â€ the same split the kernel uses for
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

    /// [`reply`](Endpoint::reply) for a service loop.
    ///
    /// A caller whose deadline passed, who canceled, or who exited has no
    /// transaction left, and the kernel answers the late reply with `-ENOENT`.
    /// That is an ordinary race that any client can trigger at will, not a fault
    /// of the service: swallow it so one impatient (or hostile) client cannot
    /// take the whole service down by hanging up before its answer. Every other
    /// error still propagates.
    pub fn reply_or_drop(&self, txn_id: u64, reply: &Parcel) -> Result<()> {
        match self.reply(txn_id, reply) {
            Err(error) if error.errno() == Some(-errno::ENOENT) => Ok(()),
            other => other,
        }
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
    /// A polling loop can avoid a fresh [`DEFAULT_BUFFER`] allocation per
    /// iteration by reusing one scratch buffer here. A message larger than `buf` is refused with
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
            first_handle: result.reserved[0],
            handles: result.reserved[1],
            first_buffer: result.reserved[2],
            buffers: result.reserved[3],
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
    /// First transferred handle installed by the delivery, as a number in this
    /// task's table. Handle `0` is a valid number, so read [`Message::handles`]
    /// to tell "none" from a real handle. The display protocol reads a client's
    /// event endpoint here.
    pub first_handle: u64,
    /// Number of handles the delivery installed (`0` = the message transferred
    /// none).
    pub handles: u64,
    /// First shared-buffer handle installed by the delivery, ready for
    /// `crate::sys::display_map_buffer`. [`Message::buffers`] says whether it
    /// is real. The display protocol reads a client's surface buffer here.
    pub first_buffer: u64,
    /// Number of shared-buffer handles the delivery installed.
    pub buffers: u64,
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
            self.endpoint.reply_or_drop(txn, &reply)?;
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
/// registry Ã¢â‚¬â€ the daemon dispatches on the parcel's interface id.
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
/// outstanding until acked or the subscriber dies) Ã¢â‚¬â€ best-effort after peer
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
/// * `system/events/service/<name>` Ã¢â‚¬â€ a service's state changed (payload:
///   `state=... pid=... status=... restarts=...`);
/// * `system/events/security/denial` Ã¢â‚¬â€ the audit counters advanced (the
///   interim signal until the kernel exposes audit records to userspace);
/// * `system/health/<name>` Ã¢â‚¬â€ retained health row published by `healthd`;
/// * `system/health/summary` Ã¢â‚¬â€ retained aggregate (worst status wins).
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
    /// The system monitor's registered name (issue #144).
    pub const SYSMOND_NAME: &str = "os.lazy.sysmond";

    /// `os.lazy.init.v1` (interim eight-byte ABI id, see [`super::topics`]).
    pub const INIT_INTERFACE: u64 = u64::from_le_bytes(*b"os.init.");
    /// `os.lazy.healthd.v1` (interim eight-byte ABI id).
    pub const HEALTHD_INTERFACE: u64 = u64::from_le_bytes(*b"os.healt");
    /// `os.lazy.logd.v1` (interim eight-byte ABI id).
    pub const LOGD_INTERFACE: u64 = u64::from_le_bytes(*b"os.logd.");
    /// `os.lazy.system.v1` (interim eight-byte ABI id).
    pub const SYSMOND_INTERFACE: u64 = u64::from_le_bytes(*b"os.sysmo");

    /// `init` methods.
    pub mod init_method {
        /// Snapshot the supervision table.
        pub const SERVICES: u32 = 1;
        /// Launch an app as a session child (issue #158).
        pub const LAUNCH: u32 = 2;
        /// Enumerate the built-in app registry (issue #158).
        pub const LIST_APPS: u32 = 3;
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

    /// `sysmond` methods (issue #144).
    pub mod sysmond_method {
        /// Return one live system-stats snapshot.
        pub const SNAPSHOT: u32 = 1;
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
        /// Fixed-layout `sysinfo` snapshot bytes (issue #144).
        pub const SYSDATA: u16 = 20;
        /// App id (`LIST_APPS` row, `LAUNCH` request).
        pub const APP: u16 = 21;
        /// Display name (`LIST_APPS` row).
        pub const APP_NAME: u16 = 22;
        /// ELF path resolved from the app id (`LIST_APPS` row).
        pub const APP_PATH: u16 = 23;
        /// Default restart policy (`always`/`on-failure`/`once`).
        pub const APP_RESTART: u16 = 24;
        /// One MIME verb the app handles (repeated).
        pub const APP_VERBS: u16 = 25;
        /// Launch argument string (`LAUNCH` request).
        pub const ARGS: u16 = 26;
        /// Session to launch into (`LAUNCH`; 0 = the caller's own).
        pub const SESSION: u16 = 27;
        /// One app record (`LIST_APPS` reply).
        pub const APP_INFO: u16 = 28;
        /// Structured error (issue #158).
        pub const ERROR: u16 = 29;
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

    /// One row of `init`'s built-in app registry (issue #158): the S5 start
    /// menu's enumeration unit and the resolution table `LAUNCH` uses.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct AppInfo {
        /// App id: the lowercase program stem (`top` -> `TOP.ELF`).
        pub id: String,
        /// Display name for menus.
        pub name: String,
        /// On-disk ELF path.
        pub path: String,
        /// Default restart policy (`always`/`on-failure`/`once`).
        pub restart: String,
        /// MIME verbs the app handles (`open`, `edit`, `reveal`).
        pub verbs: Vec<String>,
    }

    /// The outcome of `init`'s `Launch`.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct LaunchResult {
        /// App id that was launched.
        pub app: String,
        /// Task slot of the spawned child.
        pub pid: u64,
        /// Session the child was stamped with.
        pub session: u64,
    }

    /// A decoded `init` `Launch` request.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct LaunchRequest {
        /// App id from the registry.
        pub app: String,
        /// Argument string passed to the app (may be empty).
        pub args: String,
        /// Target session; `0` means the caller's own session.
        pub session: u64,
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

    /// `init`'s `ListApps` request (issue #158).
    pub fn list_apps_request() -> Parcel {
        Parcel {
            header: header(INIT_INTERFACE, init_method::LIST_APPS),
            ..Parcel::default()
        }
    }

    /// Encode `init`'s `ListApps` reply: one `APP_INFO` record per app.
    pub fn list_apps_reply(apps: &[AppInfo]) -> Result<Parcel> {
        let mut body = Encoder::new();
        for app in apps {
            let mut record = Encoder::new();
            record.string(field::APP, &app.id).map_err(Error::Parcel)?;
            record
                .string(field::APP_NAME, &app.name)
                .map_err(Error::Parcel)?;
            record
                .string(field::APP_PATH, &app.path)
                .map_err(Error::Parcel)?;
            record
                .string(field::APP_RESTART, &app.restart)
                .map_err(Error::Parcel)?;
            for verb in &app.verbs {
                record
                    .string(field::APP_VERBS, verb)
                    .map_err(Error::Parcel)?;
            }
            body.record(field::APP_INFO, &record)
                .map_err(Error::Parcel)?;
        }
        Ok(Parcel {
            header: header(INIT_INTERFACE, init_method::LIST_APPS),
            body: body.finish(),
            ..Parcel::default()
        })
    }

    /// `init`'s `Launch` request: `(app_id, args, session)`. `session` 0 means
    /// the caller's own session; only the session's owner (or root) may launch
    /// into it.
    pub fn launch_request(app: &str, args: &str, session: u64) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::APP, app).map_err(Error::Parcel)?;
        if !args.is_empty() {
            body.string(field::ARGS, args).map_err(Error::Parcel)?;
        }
        body.u64(field::SESSION, session).map_err(Error::Parcel)?;
        Ok(Parcel {
            header: header(INIT_INTERFACE, init_method::LAUNCH),
            body: body.finish(),
            ..Parcel::default()
        })
    }

    /// Encode `init`'s `Launch` reply.
    pub fn launch_reply(result: &LaunchResult) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::APP, &result.app)
            .map_err(Error::Parcel)?;
        body.u64(field::PID, result.pid).map_err(Error::Parcel)?;
        body.u64(field::SESSION, result.session)
            .map_err(Error::Parcel)?;
        Ok(Parcel {
            header: header(INIT_INTERFACE, init_method::LAUNCH),
            body: body.finish(),
            ..Parcel::default()
        })
    }

    /// Decode a `ListApps` reply.
    pub fn decode_apps(parcel: &Parcel) -> Result<Vec<AppInfo>> {
        let mut apps = Vec::new();
        for_each_record(parcel, |mut nested| {
            let mut app = AppInfo::default();
            while let Ok(Some(field)) = nested.next() {
                match (field.kind, field.id) {
                    (Kind::String, field::APP) => {
                        app.id = String::from(field.as_str().map_err(Error::Parcel)?)
                    }
                    (Kind::String, field::APP_NAME) => {
                        app.name = String::from(field.as_str().map_err(Error::Parcel)?)
                    }
                    (Kind::String, field::APP_PATH) => {
                        app.path = String::from(field.as_str().map_err(Error::Parcel)?)
                    }
                    (Kind::String, field::APP_RESTART) => {
                        app.restart = String::from(field.as_str().map_err(Error::Parcel)?)
                    }
                    (Kind::String, field::APP_VERBS) => app
                        .verbs
                        .push(String::from(field.as_str().map_err(Error::Parcel)?)),
                    _ => {}
                }
            }
            apps.push(app);
            Ok(())
        })?;
        Ok(apps)
    }

    /// Decode a `Launch` request.
    pub fn decode_launch_request(parcel: &Parcel) -> Result<LaunchRequest> {
        let mut request = LaunchRequest::default();
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            match (field.kind, field.id) {
                (Kind::String, field::APP) => {
                    request.app = String::from(field.as_str().map_err(Error::Parcel)?)
                }
                (Kind::String, field::ARGS) => {
                    request.args = String::from(field.as_str().map_err(Error::Parcel)?)
                }
                (Kind::U64, field::SESSION) => {
                    request.session = field.as_u64().map_err(Error::Parcel)?
                }
                _ => {}
            }
        }
        if request.app.is_empty() {
            return Err(Error::Errno(-errno::EINVAL));
        }
        Ok(request)
    }

    /// Decode a `Launch` reply.
    pub fn decode_launch(parcel: &Parcel) -> Result<LaunchResult> {
        let mut result = LaunchResult::default();
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            match (field.kind, field.id) {
                (Kind::String, field::APP) => {
                    result.app = String::from(field.as_str().map_err(Error::Parcel)?)
                }
                (Kind::U64, field::PID) => result.pid = field.as_u64().map_err(Error::Parcel)?,
                (Kind::U64, field::SESSION) => {
                    result.session = field.as_u64().map_err(Error::Parcel)?
                }
                _ => {}
            }
        }
        if result.app.is_empty() {
            return Err(Error::Errno(-errno::EINVAL));
        }
        Ok(result)
    }

    /// `init`'s error answer: errno-style code plus friendly text, the same
    /// shape [`super::mime::error_reply`] uses. The client turns the code back
    /// into [`Error::Init`].
    pub fn init_error_reply(method: u32, error: Error) -> Parcel {
        let code = error.errno().map(|code| -code).unwrap_or(errno::EINVAL);
        let mut body = Encoder::new();
        // A structured error field cannot overflow a fresh encoder here.
        let _ = body.error(field::ERROR, code as u32, error.message());
        Parcel {
            header: header(INIT_INTERFACE, method),
            body: body.finish(),
            ..Parcel::default()
        }
    }

    /// The first structured error field, when the reply is a service failure.
    pub fn error_field(parcel: &Parcel) -> Result<Option<i64>> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind == Kind::Error && field.id == field::ERROR {
                let (code, _message) = field.error_parts().map_err(Error::Parcel)?;
                return Ok(Some(code as i64));
            }
        }
        Ok(None)
    }

    /// Call `init`'s `ListApps`.
    ///
    /// Allocates the reply buffer per call; a polling loop should use
    /// [`fetch_apps_with`] and reuse one buffer.
    pub fn fetch_apps(endpoint: &Endpoint) -> Result<Vec<AppInfo>> {
        let mut buf = alloc::vec![0u8; super::DEFAULT_BUFFER];
        fetch_apps_with(endpoint, &mut buf)
    }

    /// [`fetch_apps`] with a caller-owned reply buffer.
    pub fn fetch_apps_with(endpoint: &Endpoint, buf: &mut [u8]) -> Result<Vec<AppInfo>> {
        let reply = endpoint.call_with(&list_apps_request(), buf, None)?;
        if let Some(code) = error_field(&reply)? {
            return Err(Error::Init(code));
        }
        decode_apps(&reply)
    }

    /// Call `init`'s `Launch` and fail on a supervisor error.
    pub fn launch(
        endpoint: &Endpoint,
        app: &str,
        args: &str,
        session: u64,
    ) -> Result<LaunchResult> {
        let reply = endpoint.call(&launch_request(app, args, session)?, None)?;
        if let Some(code) = error_field(&reply)? {
            return Err(Error::Init(code));
        }
        decode_launch(&reply)
    }

    /// Resolve [`INIT_NAME`] and launch `app` (a convenience for CLI callers;
    /// a polling loop should hold its own endpoint).
    pub fn launch_app(app: &str, args: &str, session: u64) -> Result<LaunchResult> {
        let endpoint = resolve_service(INIT_NAME)?;
        launch(&endpoint, app, args, session)
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

    /// `sysmond`'s `Snapshot` request (issue #144).
    pub fn sysinfo_request() -> Parcel {
        Parcel {
            header: header(SYSMOND_INTERFACE, sysmond_method::SNAPSHOT),
            ..Parcel::default()
        }
    }

    /// Encode `sysmond`'s `Snapshot` reply: the raw fixed-layout `sysinfo`
    /// words as one bytes field.
    pub fn sysinfo_reply(snapshot: &crate::sysinfo::Snapshot) -> Result<Parcel> {
        let mut wire = [0u8; crate::sysinfo::SIZE];
        if !snapshot.write_bytes(&mut wire) {
            return Err(Error::Errno(-errno::E2BIG));
        }
        let mut body = Encoder::new();
        body.bytes(field::SYSDATA, &wire).map_err(Error::Parcel)?;
        Ok(Parcel {
            header: header(SYSMOND_INTERFACE, sysmond_method::SNAPSHOT),
            body: body.finish(),
            ..Parcel::default()
        })
    }

    /// A service's error answer for a request on `interface_id`/`method`: the
    /// errno-style code plus friendly text in a structured [`field::ERROR`].
    pub fn error_reply(interface_id: u64, method: u32, error: Error) -> Parcel {
        let code = error.errno().map(|code| -code).unwrap_or(errno::EINVAL);
        let mut body = Encoder::new();
        // A structured error field cannot overflow a fresh encoder here.
        let _ = body.error(field::ERROR, code as u32, error.message());
        Parcel {
            header: header(interface_id, method),
            body: body.finish(),
            ..Parcel::default()
        }
    }

    /// Call `sysmond`'s `Snapshot` and decode the fixed-layout reply; a
    /// service failure comes back as its original errno.
    pub fn fetch_sysinfo(endpoint: &Endpoint) -> Result<crate::sysinfo::Snapshot> {
        let reply = endpoint.call(&sysinfo_request(), None)?;
        let mut decoder = Decoder::new(&reply.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind == Kind::Bytes && field.id == field::SYSDATA {
                return crate::sysinfo::decode_bytes(field.payload)
                    .ok_or(Error::Errno(-errno::EINVAL));
            }
            if field.kind == Kind::Error && field.id == field::ERROR {
                let (code, _message) = field.error_parts().map_err(Error::Parcel)?;
                return Err(Error::Errno(-(code as i64)));
            }
        }
        Err(Error::Errno(-errno::EINVAL))
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
// keyd: the secrets and crypto service (issue #102)
// ---------------------------------------------------------------------------

/// Client and wire shapes for `keyd`, the secrets and crypto service
/// (`docs/security-model.md` section 8).
///
/// The service owns password verifiers and key material in its own memory; the
/// protocol below only ever carries *operations* and their public results.
/// There is deliberately no request that returns key material and no reply
/// that carries a verifier: a `Wrap` returns a blob the client may store but
/// cannot open, an `Unwrap` happens inside `keyd`, and `Sign` returns a tag.
/// The kernel's `SHARE_ONLY` buffers back this contract once the userspace
/// buffer syscall lands (the kernel test proves the mapping rule today) and
/// `keyd` documents the interim copy-free path.
///
/// The same module is the daemon's protocol layer, so requests, replies and
/// error shapes round-trip through one implementation.
pub mod keyd {
    use alloc::string::String;
    use alloc::vec::Vec;

    use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

    use super::{errno, registry, Endpoint, Error, Result};

    /// Registered service name.
    pub const NAME: &str = "os.lazy.keyd";

    /// `os.lazy.keyd.v1`'s interim eight-byte ABI id (the pattern the other
    /// interim service interfaces use; a `midlc` hash replaces it when the IDL
    /// owns this surface).
    pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.keyd.");

    /// Keyd methods.
    pub mod method {
        /// Check a username/password pair against the stored Argon2id verifier.
        pub const VERIFY: u32 = 1;
        /// HMAC-SHA256 `digest` under a stored key; returns the tag.
        pub const SIGN: u32 = 2;
        /// Seal `data` under a stored key; returns an authenticated blob.
        pub const WRAP: u32 = 3;
        /// Open a blob produced by `Wrap`; returns the plaintext.
        pub const UNWRAP: u32 = 4;
        /// Cryptographically strong bytes.
        pub const RANDOM: u32 = 5;
        /// Create a fresh random key of a named type; returns its id.
        pub const GENERATE: u32 = 6;
        /// List key ids, types, and use counters (never material).
        pub const LIST: u32 = 7;
        /// Round-trip probe.
        pub const PING: u32 = 8;
        /// Install (or replace) an account's password verifier. Root only:
        /// the accounts service pushes its database here so `Verify` can
        /// answer for every account, not just the built-in demo one.
        pub const PROVISION: u32 = 9;
    }

    /// Protocol TLV field ids.
    pub mod field {
        /// Account name for `Verify` / `Provision`.
        pub const USER: u16 = 1;
        /// Plaintext secret for `Verify` (crosses the channel; the kernel
        /// stamps the sender so `keyd` can audit who asked).
        pub const SECRET: u16 = 2;
        /// Key id naming a stored key.
        pub const KEY: u16 = 3;
        /// Digest bytes for `Sign`.
        pub const DIGEST: u16 = 4;
        /// Plaintext for `Wrap` / ciphertext blob for `Unwrap`.
        pub const DATA: u16 = 5;
        /// Number of bytes for `Random`.
        pub const LEN: u16 = 6;
        /// Key type name for `Generate`.
        pub const KIND: u16 = 7;
        /// New key id.
        pub const ID: u16 = 8;
        /// Opaque bytes (random output, tag, wrapped blob).
        pub const BYTES: u16 = 9;
        /// Boolean result (`Verify`).
        pub const OK: u16 = 10;
        /// One key-list record.
        pub const ENTRY: u16 = 11;
        /// Key use counter.
        pub const USES: u16 = 12;
        /// Tick of the last use.
        pub const LAST_USE: u16 = 13;
        /// Structured error reply.
        pub const ERROR: u16 = 14;
    }

    /// Key type `Generate` accepts for HMAC keys (`Sign`).
    pub const KIND_HMAC: &str = "hmac";
    /// Key type `Generate` accepts for wrapping keys (`Wrap`/`Unwrap`).
    pub const KIND_WRAP: &str = "wrap";

    /// Largest plaintext, blob or random payload in one request or reply.
    /// Sized well below the 16 KiB call buffer so a reply always fits.
    pub const MAX_BYTES: usize = 8 * 1024;

    /// One row of the key list: identity and counters only, never material.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct KeyInfo {
        /// Opaque key id clients pass back in `Sign`/`Wrap`/`Unwrap`.
        pub id: u64,
        /// Key type name.
        pub kind: String,
        /// Operations this key has served.
        pub uses: u64,
        /// PIT tick of the last use (`0` before the first).
        pub last_use: u64,
    }

    /// A header for a keyd parcel of `method`.
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

    /// Wrap an encoded body in a keyd parcel.
    fn request_parcel(method: u32, body: Encoder) -> Parcel {
        Parcel {
            header: header(method),
            body: body.finish(),
            handles: Vec::new(),
            buffers: Vec::new(),
        }
    }

    /// `Verify(user, secret)`.
    pub fn verify_request(user: &str, secret: &str) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::USER, user).map_err(Error::Parcel)?;
        body.string(field::SECRET, secret).map_err(Error::Parcel)?;
        Ok(request_parcel(method::VERIFY, body))
    }

    /// `Provision(user, secret)`: root-only; `keyd` derives and stores the
    /// Argon2id verifier, and the secret does not outlive the call.
    pub fn provision_request(user: &str, secret: &str) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::USER, user).map_err(Error::Parcel)?;
        body.string(field::SECRET, secret).map_err(Error::Parcel)?;
        Ok(request_parcel(method::PROVISION, body))
    }

    /// `Sign(key, digest)`.
    pub fn sign_request(key: u64, digest: &[u8]) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::KEY, key).map_err(Error::Parcel)?;
        body.bytes(field::DIGEST, digest).map_err(Error::Parcel)?;
        Ok(request_parcel(method::SIGN, body))
    }

    /// `Wrap(key, bytes)`.
    pub fn wrap_request(key: u64, plaintext: &[u8]) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::KEY, key).map_err(Error::Parcel)?;
        body.bytes(field::DATA, plaintext).map_err(Error::Parcel)?;
        Ok(request_parcel(method::WRAP, body))
    }

    /// `Unwrap(key, blob)`.
    pub fn unwrap_request(key: u64, blob: &[u8]) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::KEY, key).map_err(Error::Parcel)?;
        body.bytes(field::DATA, blob).map_err(Error::Parcel)?;
        Ok(request_parcel(method::UNWRAP, body))
    }

    /// `Random(len)`.
    pub fn random_request(len: u64) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::LEN, len).map_err(Error::Parcel)?;
        Ok(request_parcel(method::RANDOM, body))
    }

    /// `GenerateKey(type)`.
    pub fn generate_request(kind: &str) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::KIND, kind).map_err(Error::Parcel)?;
        Ok(request_parcel(method::GENERATE, body))
    }

    /// `List`.
    pub fn list_request() -> Parcel {
        request_parcel(method::LIST, Encoder::new())
    }

    /// `Ping`.
    pub fn ping_request() -> Parcel {
        request_parcel(method::PING, Encoder::new())
    }

    /// An empty reply (Ping, or a successful void operation).
    pub fn ok_reply(method: u32) -> Parcel {
        request_parcel(method, Encoder::new())
    }

    /// A `Verify` reply carrying the boolean verdict.
    pub fn bool_reply(method: u32, ok: bool) -> Parcel {
        let mut body = Encoder::new();
        // A fresh encoder has room for one field, so this cannot fail.
        let _ = body.bool(field::OK, ok);
        request_parcel(method, body)
    }

    /// A reply carrying opaque bytes (tag, blob, random output).
    pub fn bytes_reply(method: u32, bytes: &[u8]) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.bytes(field::BYTES, bytes).map_err(Error::Parcel)?;
        Ok(request_parcel(method, body))
    }

    /// A reply carrying a key id.
    pub fn id_reply(method: u32, id: u64) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::ID, id).map_err(Error::Parcel)?;
        Ok(request_parcel(method, body))
    }

    /// A `List` reply: one `ENTRY` record per key.
    pub fn keys_reply(keys: &[KeyInfo]) -> Result<Parcel> {
        let mut body = Encoder::new();
        for key in keys {
            let mut record = Encoder::new();
            record.u64(field::KEY, key.id).map_err(Error::Parcel)?;
            record
                .string(field::KIND, &key.kind)
                .map_err(Error::Parcel)?;
            record.u64(field::USES, key.uses).map_err(Error::Parcel)?;
            record
                .u64(field::LAST_USE, key.last_use)
                .map_err(Error::Parcel)?;
            body.record(field::ENTRY, &record).map_err(Error::Parcel)?;
        }
        Ok(request_parcel(method::LIST, body))
    }

    /// The daemon's error answer: errno-style code plus friendly text. The
    /// client turns the code back into [`Error::Errno`].
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

    /// The first string field with `id`, if any.
    pub fn string_field(parcel: &Parcel, id: u16) -> Result<Option<String>> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind == Kind::String && field.id == id {
                return Ok(Some(String::from(field.as_str().map_err(Error::Parcel)?)));
            }
        }
        Ok(None)
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

    /// Decode a `Verify` reply.
    pub fn decode_bool(parcel: &Parcel) -> Result<bool> {
        bool_field(parcel, field::OK)
    }

    /// Decode a reply carrying a key id.
    pub fn decode_id(parcel: &Parcel) -> Result<u64> {
        u64_field(parcel, field::ID)?.ok_or(Error::Errno(-errno::EINVAL))
    }

    /// Decode a reply carrying opaque bytes.
    pub fn decode_bytes(parcel: &Parcel) -> Result<Vec<u8>> {
        bytes_field(parcel, field::BYTES)?.ok_or(Error::Errno(-errno::EINVAL))
    }

    /// Decode a `List` reply.
    pub fn decode_keys(parcel: &Parcel) -> Result<Vec<KeyInfo>> {
        let mut keys = Vec::new();
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(record) = decoder.next().map_err(Error::Parcel)? {
            if record.kind != Kind::Struct || record.id != field::ENTRY {
                continue;
            }
            let mut nested = record.nested(0).map_err(Error::Parcel)?;
            let mut key = KeyInfo::default();
            while let Some(item) = nested.next().map_err(Error::Parcel)? {
                match (item.kind, item.id) {
                    (Kind::U64, field::KEY) => key.id = item.as_u64().map_err(Error::Parcel)?,
                    (Kind::String, field::KIND) => {
                        key.kind = String::from(item.as_str().map_err(Error::Parcel)?)
                    }
                    (Kind::U64, field::USES) => key.uses = item.as_u64().map_err(Error::Parcel)?,
                    (Kind::U64, field::LAST_USE) => {
                        key.last_use = item.as_u64().map_err(Error::Parcel)?
                    }
                    _ => {}
                }
            }
            keys.push(key);
        }
        Ok(keys)
    }

    /// A client of the `keyd` service.
    ///
    /// `keyd` may still be registering when a late-booting task resolves it;
    /// callers that must not fail (like the boot self-test) retry, while the
    /// interactive commands report the friendly "no service" error.
    pub struct Client {
        endpoint: Endpoint,
    }

    impl Client {
        /// Resolve [`NAME`] and wrap the service endpoint.
        pub fn connect() -> Result<Client> {
            Ok(Client {
                endpoint: registry::resolve(NAME)?,
            })
        }

        /// Wrap an already-resolved endpoint.
        pub fn from_endpoint(endpoint: Endpoint) -> Client {
            Client { endpoint }
        }

        /// The underlying service endpoint (diagnostics).
        pub fn endpoint(&self) -> Endpoint {
            self.endpoint
        }

        /// Run one request as a blocking call and fail on a daemon error reply.
        /// The request parcel already carries its method, so no separate
        /// selector is needed here.
        fn call(&self, request: &Parcel) -> Result<Parcel> {
            let reply = self.endpoint.call(request, None)?;
            if let Some(code) = error_field(&reply)? {
                return Err(Error::Errno(-code));
            }
            Ok(reply)
        }

        /// Check a username/password pair inside `keyd`; the verifier never
        /// leaves the service.
        pub fn verify(&self, user: &str, secret: &str) -> Result<bool> {
            let reply = self.call(&verify_request(user, secret)?)?;
            decode_bool(&reply)
        }

        /// Install (or replace) `user`'s password verifier inside `keyd`.
        /// Refused with `-EPERM` unless the caller is uid 0.
        pub fn provision(&self, user: &str, secret: &str) -> Result<()> {
            self.call(&provision_request(user, secret)?).map(|_| ())
        }

        /// HMAC-SHA256 `digest` under the stored key; returns the tag.
        pub fn sign(&self, key: u64, digest: &[u8]) -> Result<Vec<u8>> {
            let reply = self.call(&sign_request(key, digest)?)?;
            decode_bytes(&reply)
        }

        /// Seal `plaintext` under the stored key.
        pub fn wrap(&self, key: u64, plaintext: &[u8]) -> Result<Vec<u8>> {
            let reply = self.call(&wrap_request(key, plaintext)?)?;
            decode_bytes(&reply)
        }

        /// Open a blob produced by [`Client::wrap`] for this key.
        pub fn unwrap(&self, key: u64, blob: &[u8]) -> Result<Vec<u8>> {
            let reply = self.call(&unwrap_request(key, blob)?)?;
            decode_bytes(&reply)
        }

        /// Cryptographically strong bytes.
        pub fn random(&self, len: usize) -> Result<Vec<u8>> {
            let reply = self.call(&random_request(len as u64)?)?;
            decode_bytes(&reply)
        }

        /// Create a fresh random key of `kind`; returns its id.
        pub fn generate(&self, kind: &str) -> Result<u64> {
            let reply = self.call(&generate_request(kind)?)?;
            decode_id(&reply)
        }

        /// Key ids and last-use counters; never key material.
        pub fn keys(&self) -> Result<Vec<KeyInfo>> {
            let reply = self.call(&list_request())?;
            decode_keys(&reply)
        }

        /// Round-trip probe.
        pub fn ping(&self) -> Result<()> {
            self.call(&ping_request())?;
            Ok(())
        }
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

// ---------------------------------------------------------------------------
// Display protocol (issue #113)
// ---------------------------------------------------------------------------

/// The display protocol (`docs/platform-plan.md` S4.4, issue #113): the
/// userspace compositor `xuid` owns the framebuffer through the kernel's device
/// grant and implements one Messenger interface, `os.lazy.display.v1`.
///
/// ## Client and compositor
///
/// An app connects with [`display::Client::connect`], creates a surface (the
/// parcel transfers its **event endpoint**, so the compositor can send input
/// back), creates a shared pixel buffer with the `display` syscall, attaches it
/// and draws into it. `Commit` after each change is the "pixels are ready"
/// signal.
///
/// ## Input delivery
///
/// Input arrives as one-way messages on the event endpoint the app transferred:
/// [`display::decode_event`] turns a received [`Message`] into an
/// [`display::Event`]. The app polls that channel; the compositor forwards only
/// events for the focused surface.
///
/// ## Rendering
///
/// [`display::Canvas`] is the userspace software blitter ([`display::font`] is
/// a 5x7 bitmap font): apps and the compositor draw into the same shared-buffer
/// mapping the kernel handed out, so compositing an app's window is a plain
/// memory copy from the app buffer into the screen buffer. The tiny-skia-like
/// in-kernel renderer cannot be linked from ring 3, which is why this path is a
/// simple blitter; the XUI/tiny-skia toolkit is the S4 follow-up.
///
/// ## Shell extensions (issue #167, S5.0)
///
/// LazyShell (S5) is one more display client, so the compositor gains an
/// append-only set of methods and one-way events; older clients and older
/// compositors keep working because unknown TLV fields and methods are ignored:
///
/// * `CreateSurface` gains a `ROLE` field: [`role::WINDOW`] (the default when
///   the field is absent) or [`role::DESKTOP`]. A desktop surface paints at the
///   bottom of the z-order, above the compositor background and below every
///   window, with no chrome and no taskbar entry; creating a second one
///   replaces the first.
/// * `ListSurfaces` replies with one [`SurfaceInfo`] row per surface (id,
///   title, geometry, minimized, focused); `GetWorkArea` replies with the
///   rectangle available to windows (the fallback taskbar is excluded while it
///   is visible), and `GetTheme` reports the chrome [`Theme`] so the shell can
///   match xuid's palette.
/// * `Subscribe(role, events)` transfers the shell's event endpoint. The
///   compositor sends one-way [`ShellEvent`]s there: `SurfaceChanged` on
///   create/destroy/move/minimize/restore/title, `FocusChanged`, and
///   `StartMenu` when the global `Ctrl+Esc`/`Super` hotkey fires. The role
///   `"shell"` also hides the built-in taskbar; the compositor stays usable
///   with no shell attached.
/// * The global hotkeys live in the compositor: `Alt+Tab` shows a centered
///   overlay, cycles on repeated Tab, and commits on Alt release; `Alt+F4`
///   sends `WindowClose` to the focused surface; `Escape` cancels a drag & drop.
pub mod display {
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    use libmessenger::{flags, BufferDesc, Decoder, Encoder, Header, Kind, Parcel, VERSION};

    use super::{
        errno, op, registry, syscall, Endpoint, Error, Message, MsgArgs, MsgResult, Result,
    };

    /// Well-known compositor name. The interface id is the first eight bytes of
    /// the same string, matching the kernel registry convention.
    pub const NAME: &str = "os.lazy.display.v1";
    /// Interface id (`os.lazy.` prefix, like the registry's).
    pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.lazy.");

    /// Display protocol methods.
    pub mod method {
        /// Create a surface; reply carries its id.
        pub const CREATE_SURFACE: u32 = 1;
        /// Attach (or replace) a surface's pixel buffer.
        pub const ATTACH_BUFFER: u32 = 2;
        /// Signal that a damage rectangle is ready to present.
        pub const COMMIT: u32 = 3;
        /// Drop a surface.
        pub const DESTROY_SURFACE: u32 = 4;
        /// Compositor to app: pointer moved.
        pub const POINTER_MOVE: u32 = 5;
        /// Compositor to app: pointer button pressed.
        pub const POINTER_DOWN: u32 = 6;
        /// Compositor to app: pointer button released.
        pub const POINTER_UP: u32 = 7;
        /// Compositor to app: key pressed.
        pub const KEY_DOWN: u32 = 8;
        /// Compositor to app: key released.
        pub const KEY_UP: u32 = 9;
        /// Compositor to app: the window manager closed this surface (issue
        /// #143). One-way; the app is expected to exit (or re-create).
        pub const WINDOW_CLOSE: u32 = 10;
        /// App to compositor: begin a compositor-mediated drag carrying a
        /// clipboard token (issue #145).
        pub const DRAG_START: u32 = 11;
        /// App to compositor: cancel the drag that started at this surface.
        pub const DRAG_CANCEL: u32 = 12;
        /// Compositor to app: a drag entered this surface (`A`/`B` = local x/y).
        pub const DRAG_ENTER: u32 = 13;
        /// Compositor to app: a drag moved inside this surface (`A`/`B` = local
        /// x/y).
        pub const DRAG_OVER: u32 = 14;
        /// Compositor to app: a drag left this surface.
        pub const DRAG_LEAVE: u32 = 15;
        /// Compositor to app: a drag was released over this surface; carries
        /// `TOKEN` and `MIME`.
        pub const DROP: u32 = 16;
        /// Compositor to the source: the drag ended; `A` = 1 when dropped, 0
        /// when cancelled.
        pub const DRAG_ENDED: u32 = 17;
        /// List every surface; the reply is one row per surface (issue #167).
        pub const LIST_SURFACES: u32 = 18;
        /// The rectangle available to windows, above the fallback taskbar
        /// (issue #167).
        pub const GET_WORK_AREA: u32 = 19;
        /// Register this task as the shell subscriber; the parcel transfers an
        /// event endpoint (issue #167).
        pub const SUBSCRIBE: u32 = 20;
        /// The compositor's current chrome palette (issue #167).
        pub const GET_THEME: u32 = 21;
        /// Compositor to the shell: a surface was created, destroyed, moved,
        /// minimized, restored, or retitled (issue #167).
        pub const SURFACE_CHANGED: u32 = 22;
        /// Compositor to the shell: the focused surface changed (issue #167).
        pub const FOCUS_CHANGED: u32 = 23;
        /// Compositor to the shell: the global start-menu hotkey (Ctrl+Esc or
        /// Super) fired (issue #167).
        pub const START_MENU: u32 = 24;
    }

    /// TLV field ids of the display protocol.
    pub mod field {
        /// Surface id.
        pub const SURFACE: u16 = 1;
        /// Surface width in pixels.
        pub const WIDTH: u16 = 2;
        /// Surface height in pixels.
        pub const HEIGHT: u16 = 3;
        /// Window title string.
        pub const TITLE: u16 = 4;
        /// Damage rectangle x.
        pub const X: u16 = 5;
        /// Damage rectangle y.
        pub const Y: u16 = 6;
        /// Damage rectangle width.
        pub const W: u16 = 7;
        /// Damage rectangle height.
        pub const H: u16 = 8;
        /// Event payload, first word (key code, pointer x, or button).
        pub const A: u16 = 9;
        /// Event payload, second word (pointer y).
        pub const B: u16 = 10;
        /// Structured error code in a failure reply.
        pub const ERROR: u16 = 11;
        /// Clipboard token a drag carries (issue #145).
        pub const TOKEN: u16 = 12;
        /// MIME type string of a drag payload.
        pub const MIME: u16 = 13;
        /// Surface role in `CreateSurface` (issue #167): [`role::WINDOW`] or
        /// [`role::DESKTOP`].
        pub const ROLE: u16 = 14;
        /// Surface minimized flag in list rows and change events (issue #167).
        pub const MINIMIZED: u16 = 15;
        /// Surface focused flag in list rows (issue #167).
        pub const FOCUSED: u16 = 16;
        /// Subscriber role string in `Subscribe` (issue #167).
        pub const SUBSCRIBER_ROLE: u16 = 17;
        /// Active title-bar colour in `GetTheme`, `0xRRGGBB` (issue #167).
        pub const TITLE_BG_ACTIVE: u16 = 18;
        /// Inactive title-bar colour in `GetTheme` (issue #167).
        pub const TITLE_BG_INACTIVE: u16 = 19;
        /// Window border colour in `GetTheme` (issue #167).
        pub const BORDER: u16 = 20;
        /// Taskbar colour in `GetTheme` (issue #167).
        pub const TASKBAR: u16 = 21;
        /// Chrome text colour in `GetTheme` (issue #167).
        pub const TEXT: u16 = 22;
    }

    /// Surface roles carried in the `CreateSurface` `ROLE` field (issue #167).
    pub mod role {
        /// A regular decorated window (the default when the field is absent).
        pub const WINDOW: u64 = 0;
        /// The full-screen desktop surface, painted above the background and
        /// below every window; a new desktop replaces the current one.
        pub const DESKTOP: u64 = 1;
    }

    /// The `SURFACE_CHANGED` event kinds (issue #167).
    pub mod change {
        pub const CREATED: u64 = 1;
        pub const DESTROYED: u64 = 2;
        pub const MOVED: u64 = 3;
        pub const MINIMIZED: u64 = 4;
        pub const RESTORED: u64 = 5;
        /// The surface's title changed (reserved; xuid has no rename method yet).
        pub const TITLE: u64 = 6;
    }

    /// The subscriber role that asks xuid to hide its built-in taskbar
    /// (issue #167).
    pub const ROLE_SHELL: &str = "shell";

    /// Longest MIME string the compositor accepts in a `DragStart`.
    pub const MAX_MIME: usize = 64;

    /// Longest subscriber role string the compositor accepts in `Subscribe`.
    pub const MAX_ROLE: usize = 32;

    /// Key codes for non-character keys; mirrors `kernel/src/display.rs`.
    pub mod key {
        pub const ENTER: u32 = 13;
        pub const BACKSPACE: u32 = 8;
        pub const TAB: u32 = 9;
        pub const ESCAPE: u32 = 27;
        pub const SPACE: u32 = 32;
        pub const LEFT: u32 = 0x100;
        pub const RIGHT: u32 = 0x101;
        pub const UP: u32 = 0x102;
        pub const DOWN: u32 = 0x103;
        pub const PAGE_UP: u32 = 0x104;
        pub const PAGE_DOWN: u32 = 0x105;
        pub const HOME: u32 = 0x106;
        pub const END: u32 = 0x107;
        /// Modifier keys (issue #167). The compositor consumes them for global
        /// hotkeys and never forwards them to a client; clients that forward
        /// raw input may still decode them defensively.
        pub const SHIFT: u32 = 0x108;
        pub const CTRL: u32 = 0x109;
        pub const ALT: u32 = 0x10A;
        pub const SUPER: u32 = 0x10B;
        /// Function key 4, used for the compositor's Alt+F4 (issue #167).
        pub const F4: u32 = 0x113;
    }

    /// Pointer buttons, as reported in pointer events.
    pub mod button {
        pub const LEFT: u32 = 1;
        pub const RIGHT: u32 = 2;
        pub const MIDDLE: u32 = 3;
    }

    /// PIT ticks `Client::connect` waits for the compositor's name to appear.
    /// The kernel spawns `xuid` before its demo client, but the compositor must
    /// still bind the display and register the name, so a short retry window
    /// keeps the app robust to that race.
    const CONNECT_TICKS: u64 = 100;

    /// A header for a display parcel of `method`. `ALLOW_NESTED` keeps an app's
    /// event poll from tripping the kernel's per-channel cycle check while a
    /// `Commit` call is in flight.
    fn header(method: u32) -> Header {
        Header {
            version: VERSION,
            flags: flags::ALLOW_NESTED,
            interface_id: INTERFACE,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        }
    }

    /// An app's connection to the compositor.
    #[derive(Clone, Copy)]
    pub struct Client {
        endpoint: Endpoint,
    }

    impl Client {
        /// Resolve [`NAME`] into this task, retrying briefly while the
        /// compositor starts, and wrap the endpoint.
        pub fn connect() -> Result<Client> {
            let deadline = crate::sys::clock().saturating_add(CONNECT_TICKS);
            loop {
                match registry::resolve(NAME) {
                    Ok(endpoint) => return Ok(Client { endpoint }),
                    Err(error) => {
                        if crate::sys::clock() >= deadline {
                            return Err(error);
                        }
                        // Park one tick on the child-exit queue: no children
                        // means this is a clean sleep (the "no clock yet"
                        // pattern the other clients use).
                        let _ = crate::sys::wait(crate::sys::clock() + 1);
                    }
                }
            }
        }

        /// A `CreateSurface(width, height, title, events)` request. The event
        /// endpoint is moved to the compositor, which sends input back on it.
        /// Returns the new surface id.
        pub fn create_surface(
            &self,
            width: u64,
            height: u64,
            title: &str,
            events: &Endpoint,
        ) -> Result<u64> {
            self.create_surface_role(width, height, title, events, role::WINDOW)
        }

        /// A `CreateSurface` with [`role::DESKTOP`] (issue #167): the surface
        /// paints at the bottom of the z-order, above the background colour and
        /// below every window. It has no chrome, never takes focus and never
        /// appears in the taskbar or the Alt+Tab cycle; creating a new desktop
        /// replaces the previous one. The event endpoint is still transferred,
        /// so a future desktop can receive input.
        pub fn create_desktop_surface(
            &self,
            width: u64,
            height: u64,
            title: &str,
            events: &Endpoint,
        ) -> Result<u64> {
            self.create_surface_role(width, height, title, events, role::DESKTOP)
        }

        /// The shared body of [`Client::create_surface`] and
        /// [`Client::create_desktop_surface`].
        fn create_surface_role(
            &self,
            width: u64,
            height: u64,
            title: &str,
            events: &Endpoint,
            role: u64,
        ) -> Result<u64> {
            let mut body = Encoder::new();
            body.u64(field::WIDTH, width).map_err(Error::Parcel)?;
            body.u64(field::HEIGHT, height).map_err(Error::Parcel)?;
            body.string(field::TITLE, title).map_err(Error::Parcel)?;
            body.u64(field::ROLE, role).map_err(Error::Parcel)?;
            let parcel = Parcel {
                header: header(method::CREATE_SURFACE),
                body: body.finish(),
                handles: vec![events.handle()],
                buffers: Vec::new(),
            };
            let mut buf = [0u8; 256];
            let reply = self.endpoint.call_with(&parcel, &mut buf, None)?;
            // A refusal (e.g. `-EACCES` for the desktop role) is a structured
            // error reply, not a missing surface id.
            if let Some(code) = error_field(&reply) {
                return Err(Error::Errno(-code));
            }
            let mut decoder = Decoder::new(&reply.body);
            while let Some(field) = decoder.next().map_err(Error::Parcel)? {
                if field.kind == Kind::U64 && field.id == field::SURFACE {
                    return field.as_u64().map_err(Error::Parcel);
                }
            }
            Err(Error::Errno(-errno::EINVAL))
        }

        /// `Subscribe(role, events)`: register this task as the shell
        /// subscriber (issue #167). The event endpoint is moved to the
        /// compositor, which sends one-way [`ShellEvent`]s there. The role
        /// [`ROLE_SHELL`] also hides xuid's built-in taskbar; any other role
        /// keeps the fallback chrome. Registering again replaces the endpoint.
        pub fn subscribe(&self, role: &str, events: &Endpoint) -> Result<()> {
            let mut body = Encoder::new();
            body.string(field::SUBSCRIBER_ROLE, role)
                .map_err(Error::Parcel)?;
            let parcel = Parcel {
                header: header(method::SUBSCRIBE),
                body: body.finish(),
                handles: vec![events.handle()],
                buffers: Vec::new(),
            };
            let mut buf = [0u8; 256]; // an error reply carries a message
            let reply = self.endpoint.call_with(&parcel, &mut buf, None)?;
            match error_field(&reply) {
                Some(code) => Err(Error::Errno(-code)),
                None => Ok(()),
            }
        }

        /// `ListSurfaces`: every surface the compositor knows, in its z-order
        /// (bottom first). Desktop surfaces are included and their rows show
        /// the composited geometry (issue #167).
        pub fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
            let parcel = Parcel {
                header: header(method::LIST_SURFACES),
                body: Encoder::new().finish(),
                handles: Vec::new(),
                buffers: Vec::new(),
            };
            let reply = self.endpoint.call(&parcel, None)?;
            if let Some(code) = error_field(&reply) {
                return Err(Error::Errno(-code));
            }
            decode_surface_list(&reply.body)
        }

        /// `GetWorkArea`: the rectangle windows may occupy. While the built-in
        /// fallback taskbar is visible the bar's strip is excluded; with a
        /// shell registered (`Subscribe("shell", ..)`) the bar is hidden and
        /// the work area is the whole screen (issue #167).
        pub fn get_work_area(&self) -> Result<Rect> {
            let parcel = Parcel {
                header: header(method::GET_WORK_AREA),
                body: Encoder::new().finish(),
                handles: Vec::new(),
                buffers: Vec::new(),
            };
            let mut buf = [0u8; 256];
            let reply = self.endpoint.call_with(&parcel, &mut buf, None)?;
            if let Some(code) = error_field(&reply) {
                return Err(Error::Errno(-code));
            }
            let (mut x, mut y, mut w, mut h) = (0i32, 0i32, 0i32, 0i32);
            let mut decoder = Decoder::new(&reply.body);
            while let Some(field) = decoder.next().map_err(Error::Parcel)? {
                if field.kind != Kind::U64 {
                    continue;
                }
                let value = field.as_u64().map_err(Error::Parcel)? as i32;
                match field.id {
                    field::X => x = value,
                    field::Y => y = value,
                    field::W => w = value,
                    field::H => h = value,
                    _ => {}
                }
            }
            Ok(Rect::new(x, y, w, h))
        }

        /// `GetTheme`: xuid's current chrome palette, so the shell's own
        /// surfaces can match it (issue #167).
        pub fn get_theme(&self) -> Result<Theme> {
            let parcel = Parcel {
                header: header(method::GET_THEME),
                body: Encoder::new().finish(),
                handles: Vec::new(),
                buffers: Vec::new(),
            };
            let mut buf = [0u8; 256];
            let reply = self.endpoint.call_with(&parcel, &mut buf, None)?;
            if let Some(code) = error_field(&reply) {
                return Err(Error::Errno(-code));
            }
            let mut theme = Theme::default();
            let mut decoder = Decoder::new(&reply.body);
            while let Some(field) = decoder.next().map_err(Error::Parcel)? {
                if field.kind != Kind::U64 {
                    continue;
                }
                let color = color_from_u64(field.as_u64().map_err(Error::Parcel)?);
                match field.id {
                    field::TITLE_BG_ACTIVE => theme.title_bg_active = color,
                    field::TITLE_BG_INACTIVE => theme.title_bg_inactive = color,
                    field::BORDER => theme.border = color,
                    field::TASKBAR => theme.taskbar = color,
                    field::TEXT => theme.text = color,
                    _ => {}
                }
            }
            Ok(theme)
        }

        /// Share `buffer` (a handle from the `display` syscall's
        /// `create_buffer`) with the compositor as `surface`'s pixels. The
        /// sender keeps its handle and mapping; the compositor gains one.
        pub fn attach_buffer(&self, surface: u64, buffer: u64, len: u64) -> Result<()> {
            let mut body = Encoder::new();
            body.u64(field::SURFACE, surface).map_err(Error::Parcel)?;
            let parcel = Parcel {
                header: header(method::ATTACH_BUFFER),
                body: body.finish(),
                handles: Vec::new(),
                buffers: vec![BufferDesc {
                    handle: buffer,
                    offset: 0,
                    len,
                    flags: 0,
                }],
            };
            let mut buf = [0u8; 256]; // an error reply carries a message
            self.endpoint.call_with(&parcel, &mut buf, None)?;
            Ok(())
        }

        /// Tell the compositor the `damage` rectangle of `surface` is ready.
        pub fn commit(&self, surface: u64, damage: Rect) -> Result<()> {
            let mut body = Encoder::new();
            body.u64(field::SURFACE, surface).map_err(Error::Parcel)?;
            body.u64(field::X, damage.x.max(0) as u64)
                .map_err(Error::Parcel)?;
            body.u64(field::Y, damage.y.max(0) as u64)
                .map_err(Error::Parcel)?;
            body.u64(field::W, damage.w.max(0) as u64)
                .map_err(Error::Parcel)?;
            body.u64(field::H, damage.h.max(0) as u64)
                .map_err(Error::Parcel)?;
            let parcel = Parcel {
                header: header(method::COMMIT),
                body: body.finish(),
                handles: Vec::new(),
                buffers: Vec::new(),
            };
            let mut buf = [0u8; 256]; // an error reply carries a message
            self.endpoint.call_with(&parcel, &mut buf, None)?;
            Ok(())
        }

        /// Drop `surface`; the compositor forgets it and repaints.
        pub fn destroy_surface(&self, surface: u64) -> Result<()> {
            let mut body = Encoder::new();
            body.u64(field::SURFACE, surface).map_err(Error::Parcel)?;
            let parcel = Parcel {
                header: header(method::DESTROY_SURFACE),
                body: body.finish(),
                handles: Vec::new(),
                buffers: Vec::new(),
            };
            let mut buf = [0u8; 256]; // an error reply carries a message
            self.endpoint.call_with(&parcel, &mut buf, None)?;
            Ok(())
        }

        /// `DragStart(surface, token, mime)`: hand `surface`'s in-progress
        /// gesture to the compositor, which tracks the pointer and delivers a
        /// `Drop` carrying `token`. The payload is offered to `clipboardd`
        /// first (issue #145); the compositor never sees the bytes.
        pub fn drag_start(&self, surface: u64, token: u64, mime: &str) -> Result<()> {
            let mut body = Encoder::new();
            body.u64(field::SURFACE, surface).map_err(Error::Parcel)?;
            body.u64(field::TOKEN, token).map_err(Error::Parcel)?;
            body.string(field::MIME, mime).map_err(Error::Parcel)?;
            let parcel = Parcel {
                header: header(method::DRAG_START),
                body: body.finish(),
                handles: Vec::new(),
                buffers: Vec::new(),
            };
            let mut buf = [0u8; 256]; // an error reply carries a message
            let reply = self.endpoint.call_with(&parcel, &mut buf, None)?;
            match error_field(&reply) {
                Some(code) => Err(Error::Errno(-code)),
                None => Ok(()),
            }
        }

        /// `DragCancel(surface)`: cancel the drag that started at `surface`.
        pub fn drag_cancel(&self, surface: u64) -> Result<()> {
            let mut body = Encoder::new();
            body.u64(field::SURFACE, surface).map_err(Error::Parcel)?;
            let parcel = Parcel {
                header: header(method::DRAG_CANCEL),
                body: body.finish(),
                handles: Vec::new(),
                buffers: Vec::new(),
            };
            let mut buf = [0u8; 256]; // an error reply carries a message
            let reply = self.endpoint.call_with(&parcel, &mut buf, None)?;
            match error_field(&reply) {
                Some(code) => Err(Error::Errno(-code)),
                None => Ok(()),
            }
        }
    }

    /// The structured error code in a reply, when the compositor refused a
    /// call (a positive errno, as `xuid` stores it).
    fn error_field(parcel: &Parcel) -> Option<i64> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Ok(Some(field)) = decoder.next() {
            if field.kind == Kind::Error && field.id == field::ERROR {
                let (code, _message) = field.error_parts().ok()?;
                return Some(code as i64);
            }
        }
        None
    }

    /// An input event delivered to an app by the compositor.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum EventKind {
        PointerMove,
        PointerDown,
        PointerUp,
        KeyDown,
        KeyUp,
    }

    /// One decoded input event. `a`/`b` carry: pointer `(x, y)`, button id, or
    /// key code, depending on the kind.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Event {
        pub kind: EventKind,
        pub a: i64,
        pub b: i64,
    }

    /// Decode an input event from a received message, or `None` when the
    /// message is not a display event.
    pub fn decode_event(message: &Message) -> Option<Event> {
        let kind = match message.method() {
            method::POINTER_MOVE => EventKind::PointerMove,
            method::POINTER_DOWN => EventKind::PointerDown,
            method::POINTER_UP => EventKind::PointerUp,
            method::KEY_DOWN => EventKind::KeyDown,
            method::KEY_UP => EventKind::KeyUp,
            _ => return None,
        };
        let mut a = 0i64;
        let mut b = 0i64;
        let mut decoder = Decoder::new(&message.parcel.body);
        while let Ok(Some(field)) = decoder.next() {
            if field.kind != Kind::U64 {
                continue;
            }
            match field.id {
                field::A => a = field.as_u64().ok()? as i64,
                field::B => b = field.as_u64().ok()? as i64,
                _ => {}
            }
        }
        Some(Event { kind, a, b })
    }

    /// The kind of a drag event the compositor delivers (issue #145).
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum DragKind {
        /// The drag entered this surface.
        Enter,
        /// The drag moved inside this surface.
        Over,
        /// The drag left this surface.
        Leave,
        /// The drag was released over this surface.
        Drop,
        /// (Source only) the drag ended: dropped or cancelled.
        Ended,
    }

    /// One decoded drag event. `x`/`y` are surface-relative for enter, over and
    /// drop; `token`/`mime` are set on a drop; `dropped` is set on an ended.
    #[derive(Clone, PartialEq, Eq, Debug)]
    pub struct DragEvent {
        pub kind: DragKind,
        pub x: i64,
        pub y: i64,
        pub token: u64,
        pub mime: String,
        pub dropped: bool,
    }

    /// Decode a drag event from a received message, or `None` when the message
    /// is not one. [`decode_event`] still handles input events.
    pub fn decode_drag_event(message: &Message) -> Option<DragEvent> {
        let kind = match message.method() {
            method::DRAG_ENTER => DragKind::Enter,
            method::DRAG_OVER => DragKind::Over,
            method::DRAG_LEAVE => DragKind::Leave,
            method::DROP => DragKind::Drop,
            method::DRAG_ENDED => DragKind::Ended,
            _ => return None,
        };
        let mut event = DragEvent {
            kind,
            x: 0,
            y: 0,
            token: 0,
            mime: String::new(),
            dropped: false,
        };
        let mut decoder = Decoder::new(&message.parcel.body);
        while let Ok(Some(field)) = decoder.next() {
            match (field.kind, field.id) {
                (Kind::U64, field::A) => event.x = field.as_u64().ok()? as i64,
                (Kind::U64, field::B) => event.y = field.as_u64().ok()? as i64,
                (Kind::U64, field::TOKEN) => event.token = field.as_u64().ok()?,
                (Kind::String, field::MIME) => {
                    event.mime = String::from(field.as_str().ok()?);
                }
                _ => {}
            }
        }
        if event.kind == DragKind::Ended {
            event.dropped = event.x != 0;
        }
        Some(event)
    }

    /// One row of a `ListSurfaces` reply (issue #167).
    #[derive(Clone, PartialEq, Eq, Debug)]
    pub struct SurfaceInfo {
        /// Protocol surface id.
        pub id: u64,
        /// Window title from `CreateSurface`.
        pub title: String,
        /// Window origin (the decorated window's top-left for a window; the
        /// surface origin for a desktop).
        pub x: i32,
        pub y: i32,
        /// Window content size in pixels.
        pub w: i32,
        pub h: i32,
        /// Hidden by the minimize button.
        pub minimized: bool,
        /// The compositor's focused surface.
        pub focused: bool,
        /// One of [`role`] (issue #175): lets a shell tell the desktop from a
        /// window.
        pub role: u64,
    }

    /// Decode a `ListSurfaces` reply body into rows. Rows are delimited by the
    /// `SURFACE` field, so unknown fields between rows are ignored and a newer
    /// compositor can add fields without breaking this decoder.
    pub fn decode_surface_list(body: &[u8]) -> Result<Vec<SurfaceInfo>> {
        let mut rows: Vec<SurfaceInfo> = Vec::new();
        let mut current: Option<SurfaceInfo> = None;
        let mut decoder = Decoder::new(body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            match (field.kind, field.id) {
                (Kind::U64, field::SURFACE) => {
                    if let Some(row) = current.take() {
                        rows.push(row);
                    }
                    current = Some(SurfaceInfo {
                        id: field.as_u64().map_err(Error::Parcel)?,
                        title: String::new(),
                        x: 0,
                        y: 0,
                        w: 0,
                        h: 0,
                        minimized: false,
                        focused: false,
                        role: role::WINDOW,
                    });
                }
                (Kind::String, field::TITLE) => {
                    if let Some(row) = current.as_mut() {
                        row.title = String::from(field.as_str().map_err(Error::Parcel)?);
                    }
                }
                (Kind::U64, field::X) => {
                    if let Some(row) = current.as_mut() {
                        row.x = field.as_u64().map_err(Error::Parcel)? as i32;
                    }
                }
                (Kind::U64, field::Y) => {
                    if let Some(row) = current.as_mut() {
                        row.y = field.as_u64().map_err(Error::Parcel)? as i32;
                    }
                }
                (Kind::U64, field::W) => {
                    if let Some(row) = current.as_mut() {
                        row.w = field.as_u64().map_err(Error::Parcel)? as i32;
                    }
                }
                (Kind::U64, field::H) => {
                    if let Some(row) = current.as_mut() {
                        row.h = field.as_u64().map_err(Error::Parcel)? as i32;
                    }
                }
                (Kind::U64, field::MINIMIZED) => {
                    if let Some(row) = current.as_mut() {
                        row.minimized = field.as_u64().map_err(Error::Parcel)? != 0;
                    }
                }
                (Kind::U64, field::FOCUSED) => {
                    if let Some(row) = current.as_mut() {
                        row.focused = field.as_u64().map_err(Error::Parcel)? != 0;
                    }
                }
                (Kind::U64, field::ROLE) => {
                    if let Some(row) = current.as_mut() {
                        row.role = field.as_u64().map_err(Error::Parcel)?;
                    }
                }
                _ => {}
            }
        }
        if let Some(row) = current.take() {
            rows.push(row);
        }
        Ok(rows)
    }

    /// The compositor's chrome palette (`GetTheme`, issue #167).
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Theme {
        /// Title bar of the focused window.
        pub title_bg_active: Color,
        /// Title bar of every other window.
        pub title_bg_inactive: Color,
        /// Window border.
        pub border: Color,
        /// Fallback taskbar strip.
        pub taskbar: Color,
        /// Chrome text (titles and taskbar entries).
        pub text: Color,
    }

    impl Default for Theme {
        fn default() -> Theme {
            Theme {
                title_bg_active: Color::rgb(0, 0, 0),
                title_bg_inactive: Color::rgb(0, 0, 0),
                border: Color::rgb(0, 0, 0),
                taskbar: Color::rgb(0, 0, 0),
                text: Color::rgb(0, 0, 0),
            }
        }
    }

    /// Unpack a `0xRRGGBB` theme colour.
    fn color_from_u64(value: u64) -> Color {
        Color::rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
    }

    /// One `SurfaceChanged` event (issue #167).
    #[derive(Clone, PartialEq, Eq, Debug)]
    pub struct SurfaceChanged {
        pub id: u64,
        /// One of [`change`].
        pub kind: u64,
        pub x: i32,
        pub y: i32,
        pub w: i32,
        pub h: i32,
        pub minimized: bool,
        pub focused: bool,
        /// Set on `CREATED` (the title never changes today; the `TITLE` kind
        /// carries it when a rename method lands).
        pub title: String,
        /// One of [`role`] (issue #175): lets a shell tell the desktop from a
        /// window.
        pub role: u64,
    }

    /// A one-way event for the shell subscriber (issue #167).
    #[derive(Clone, PartialEq, Eq, Debug)]
    pub enum ShellEvent {
        /// A surface was created/destroyed/moved/minimized/restored.
        SurfaceChanged(SurfaceChanged),
        /// The focused surface changed; `None` when nothing is focused.
        FocusChanged(Option<u64>),
        /// The global start-menu hotkey (Ctrl+Esc or Super) fired.
        StartMenu,
    }

    /// Decode a shell event from a received message, or `None` when the
    /// message is not one.
    pub fn decode_shell_event(message: &Message) -> Option<ShellEvent> {
        match message.method() {
            method::SURFACE_CHANGED => {
                let mut event = SurfaceChanged {
                    id: 0,
                    kind: 0,
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 0,
                    minimized: false,
                    focused: false,
                    title: String::new(),
                    role: role::WINDOW,
                };
                let mut decoder = Decoder::new(&message.parcel.body);
                while let Ok(Some(field)) = decoder.next() {
                    match (field.kind, field.id) {
                        (Kind::U64, field::SURFACE) => event.id = field.as_u64().ok()?,
                        (Kind::U64, field::A) => event.kind = field.as_u64().ok()?,
                        (Kind::U64, field::X) => event.x = field.as_u64().ok()? as i32,
                        (Kind::U64, field::Y) => event.y = field.as_u64().ok()? as i32,
                        (Kind::U64, field::W) => event.w = field.as_u64().ok()? as i32,
                        (Kind::U64, field::H) => event.h = field.as_u64().ok()? as i32,
                        (Kind::U64, field::MINIMIZED) => {
                            event.minimized = field.as_u64().ok()? != 0;
                        }
                        (Kind::U64, field::FOCUSED) => event.focused = field.as_u64().ok()? != 0,
                        (Kind::U64, field::ROLE) => event.role = field.as_u64().ok()?,
                        (Kind::String, field::TITLE) => {
                            event.title = String::from(field.as_str().ok()?);
                        }
                        _ => {}
                    }
                }
                Some(ShellEvent::SurfaceChanged(event))
            }
            method::FOCUS_CHANGED => {
                let mut id = None;
                let mut decoder = Decoder::new(&message.parcel.body);
                while let Ok(Some(field)) = decoder.next() {
                    if field.kind == Kind::U64 && field.id == field::SURFACE {
                        let value = field.as_u64().ok()?;
                        id = (value != 0).then_some(value);
                    }
                }
                Some(ShellEvent::FocusChanged(id))
            }
            method::START_MENU => Some(ShellEvent::StartMenu),
            _ => None,
        }
    }

    /// Encode a one-way event parcel into `scratch`, replacing its contents.
    ///
    /// The compositor sends events at input rates into a task whose bump
    /// allocator never frees, so it cannot build a fresh `Parcel` per event.
    /// The byte layout matches `libmessenger` exactly (header, `u64` TLV fields
    /// in order, an optional string field, no handles or buffers).
    pub fn encode_event_fields(
        scratch: &mut Vec<u8>,
        method: u32,
        fields: &[(u16, u64)],
        text: Option<(u16, &str)>,
    ) {
        scratch.clear();
        let text_len = text.map(|(_, value)| value.len()).unwrap_or(0);
        let body_len = fields.len() * 16 + if text.is_some() { 8 + text_len } else { 0 };
        scratch.extend_from_slice(&VERSION.to_le_bytes());
        scratch.extend_from_slice(&flags::ONE_WAY.to_le_bytes());
        scratch.extend_from_slice(&INTERFACE.to_le_bytes());
        scratch.extend_from_slice(&method.to_le_bytes());
        scratch.extend_from_slice(&0u64.to_le_bytes()); // txn_id
        scratch.extend_from_slice(&0u64.to_le_bytes()); // reply_to
        scratch.extend_from_slice(&0u64.to_le_bytes()); // deadline_ns
        scratch.extend_from_slice(&(body_len as u32).to_le_bytes());
        scratch.extend_from_slice(&0u16.to_le_bytes()); // handles
        scratch.extend_from_slice(&0u16.to_le_bytes()); // buffers
        for &(id, value) in fields {
            let tag = Kind::U64 as u32 | ((id as u32) << 8);
            scratch.extend_from_slice(&tag.to_le_bytes());
            scratch.extend_from_slice(&8u32.to_le_bytes());
            scratch.extend_from_slice(&value.to_le_bytes());
        }
        if let Some((id, value)) = text {
            let tag = Kind::String as u32 | ((id as u32) << 8);
            scratch.extend_from_slice(&tag.to_le_bytes());
            scratch.extend_from_slice(&(value.len() as u32).to_le_bytes());
            scratch.extend_from_slice(value.as_bytes());
        }
    }

    /// Encode an event with the two standard `A`/`B` fields.
    pub fn encode_event(scratch: &mut Vec<u8>, method: u32, a: u64, b: u64) {
        encode_event_fields(scratch, method, &[(field::A, a), (field::B, b)], None);
    }

    /// Send one input event to `endpoint` using a reusable encode buffer.
    pub fn send_event(
        endpoint: &Endpoint,
        scratch: &mut Vec<u8>,
        method: u32,
        a: u64,
        b: u64,
    ) -> Result<()> {
        encode_event(scratch, method, a, b);
        send_encoded(endpoint, scratch)
    }

    /// Send one event built by [`encode_event_fields`] to `endpoint`.
    pub fn send_event_fields(
        endpoint: &Endpoint,
        scratch: &mut Vec<u8>,
        method: u32,
        fields: &[(u16, u64)],
        text: Option<(u16, &str)>,
    ) -> Result<()> {
        encode_event_fields(scratch, method, fields, text);
        send_encoded(endpoint, scratch)
    }

    /// Send the parcel bytes already encoded in `scratch`.
    fn send_encoded(endpoint: &Endpoint, scratch: &[u8]) -> Result<()> {
        let args = MsgArgs {
            handle: endpoint.handle(),
            parcel_ptr: scratch.as_ptr() as u64,
            parcel_len: scratch.len() as u64,
            ..MsgArgs::default()
        };
        syscall(op::SEND, &args, &mut MsgResult::default())
    }

    /// An integer rectangle, used for damage and layout.
    #[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
    pub struct Rect {
        pub x: i32,
        pub y: i32,
        pub w: i32,
        pub h: i32,
    }

    impl Rect {
        pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
            Rect { x, y, w, h }
        }

        /// Whether the rectangle covers no pixels.
        pub const fn is_empty(self) -> bool {
            self.w <= 0 || self.h <= 0
        }

        /// The overlapping rectangle, empty when the two do not intersect.
        pub fn intersect(self, other: Rect) -> Rect {
            let x0 = self.x.max(other.x);
            let y0 = self.y.max(other.y);
            let x1 = (self.x + self.w).min(other.x + other.w);
            let y1 = (self.y + self.h).min(other.y + other.h);
            Rect::new(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
        }

        /// The smallest rectangle covering both.
        pub fn union(self, other: Rect) -> Rect {
            if self.is_empty() {
                return other;
            }
            if other.is_empty() {
                return self;
            }
            let x0 = self.x.min(other.x);
            let y0 = self.y.min(other.y);
            let x1 = (self.x + self.w).max(other.x + other.w);
            let y1 = (self.y + self.h).max(other.y + other.h);
            Rect::new(x0, y0, x1 - x0, y1 - y0)
        }
    }

    /// An RGB colour for the software blitter.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Color {
        pub r: u8,
        pub g: u8,
        pub b: u8,
    }

    impl Color {
        pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
            Color { r, g, b }
        }
    }

    /// A software RGBA8 blitter over a mapped shared buffer.
    ///
    /// Every write is clipped to the rectangle being drawn and to the canvas
    /// bounds, so a caller can pass an over-large damage rectangle safely.
    pub struct Canvas {
        base: *mut u8,
        width: i32,
        height: i32,
    }

    impl Canvas {
        /// Wrap the address and geometry the `display` syscall reported.
        ///
        /// # Safety
        /// `base` must be an RGBA8 mapping of at least `width * height * 4`
        /// bytes in this task's address space (the value `create_buffer` or
        /// `map_buffer` returned).
        pub unsafe fn new(base: u64, width: i32, height: i32) -> Canvas {
            Canvas {
                base: base as *mut u8,
                width,
                height,
            }
        }

        /// The canvas width in pixels.
        pub fn width(&self) -> i32 {
            self.width
        }

        /// The canvas height in pixels.
        pub fn height(&self) -> i32 {
            self.height
        }

        /// Write one pixel if it is inside the canvas and the clip rectangle.
        fn pixel(&mut self, x: i32, y: i32, color: Color, clip: Rect) {
            if x < clip.x
                || y < clip.y
                || x >= clip.x + clip.w
                || y >= clip.y + clip.h
                || x < 0
                || y < 0
                || x >= self.width
                || y >= self.height
            {
                return;
            }
            let at = ((y * self.width + x) * 4) as usize;
            // Safety: bounds were checked against the canvas geometry.
            unsafe {
                self.base.add(at).write(color.r);
                self.base.add(at + 1).write(color.g);
                self.base.add(at + 2).write(color.b);
                self.base.add(at + 3).write(0xff);
            }
        }

        /// Fill `rect` with `color`, clipped to `clip`.
        pub fn fill(&mut self, rect: Rect, clip: Rect, color: Color) {
            for y in rect.y..rect.y + rect.h {
                for x in rect.x..rect.x + rect.w {
                    self.pixel(x, y, color, clip);
                }
            }
        }

        /// Copy a tightly packed RGBA8 source image into `dst`, clipped to
        /// `clip`. `src_w` is the source row length in pixels; rows and columns
        /// past the source are ignored.
        pub fn blit(&mut self, src: &[u8], src_w: i32, src_h: i32, dst: Rect, clip: Rect) {
            for row in 0..dst.h {
                if row >= src_h {
                    break;
                }
                for col in 0..dst.w {
                    if col >= src_w {
                        break;
                    }
                    let at = ((row * src_w + col) * 4) as usize;
                    if at + 3 >= src.len() {
                        break;
                    }
                    let color = Color::rgb(src[at], src[at + 1], src[at + 2]);
                    self.pixel(dst.x + col, dst.y + row, color, clip);
                }
            }
        }

        /// Draw `text` with the 5x7 font, uppercasing as needed. `scale` is the
        /// pixel size of one font pixel (1 = 5x7, 2 = 10x14).
        pub fn text(&mut self, x: i32, y: i32, text: &str, color: Color, clip: Rect, scale: i32) {
            let scale = scale.max(1);
            let mut pen = x;
            for ch in text.chars() {
                if ch == ' ' {
                    pen += font::ADVANCE * scale;
                    continue;
                }
                if let Some(glyph) = font::glyph(ch) {
                    for (col, bits) in glyph.iter().enumerate() {
                        for row in 0..font::H {
                            if bits & (1 << row) != 0 {
                                self.fill(
                                    Rect::new(
                                        pen + col as i32 * scale,
                                        y + row * scale,
                                        scale,
                                        scale,
                                    ),
                                    clip,
                                    color,
                                );
                            }
                        }
                    }
                }
                pen += font::ADVANCE * scale;
            }
        }

        /// Draw the mouse cursor sprite with its top-left at `(x, y)`.
        ///
        /// A black outline is drawn first, then the white body, so the cursor
        /// stays visible over both bright and dark pixels.
        pub fn cursor(&mut self, x: i32, y: i32, clip: Rect) {
            for row in 0..8i32 {
                for col in 0..8i32 {
                    if font::CURSOR[row as usize] & (0x80 >> col) == 0 {
                        continue;
                    }
                    self.fill(
                        Rect::new(x + col - 1, y + row - 1, 3, 3),
                        clip,
                        Color::rgb(0, 0, 0),
                    );
                }
            }
            for row in 0..8i32 {
                for col in 0..8i32 {
                    if font::CURSOR[row as usize] & (0x80 >> col) != 0 {
                        self.fill(
                            Rect::new(x + col, y + row, 1, 1),
                            clip,
                            Color::rgb(240, 240, 240),
                        );
                    }
                }
            }
        }
    }

    /// The 5x7 bitmap font used for decorations and demo text.
    ///
    /// Each glyph is five columns; in a column byte, bit `n` is row `n` with
    /// row zero at the top. Only the characters a window title or a demo label
    /// needs are defined; anything else is skipped.
    pub mod font {
        /// Glyph height in pixels.
        pub const H: i32 = 7;
        /// Advance per character (five glyph columns plus one pixel gap).
        pub const ADVANCE: i32 = 6;

        /// The cursor sprite, one bit per pixel (MSB = leftmost).
        pub const CURSOR: [u8; 8] = [0x80, 0xC0, 0xA0, 0x90, 0x88, 0x84, 0xFC, 0xC0];

        /// Look up a glyph, upper-casing lower-case ASCII first.
        pub fn glyph(ch: char) -> Option<&'static [u8; 5]> {
            let ch = ch.to_ascii_uppercase();
            Some(match ch {
                ' ' => &[0x00, 0x00, 0x00, 0x00, 0x00],
                '-' => &[0x00, 0x08, 0x08, 0x08, 0x00],
                '.' => &[0x00, 0x00, 0x40, 0x00, 0x00],
                ':' => &[0x00, 0x00, 0x24, 0x00, 0x00],
                '/' => &[0x40, 0x30, 0x08, 0x06, 0x01],
                '+' => &[0x00, 0x08, 0x1C, 0x08, 0x00],
                '!' => &[0x00, 0x00, 0x5F, 0x00, 0x00],
                '?' => &[0x02, 0x01, 0x51, 0x09, 0x06],
                '0' => &[0x3E, 0x51, 0x49, 0x45, 0x3E],
                '1' => &[0x00, 0x42, 0x7F, 0x40, 0x00],
                '2' => &[0x42, 0x61, 0x51, 0x49, 0x46],
                '3' => &[0x22, 0x41, 0x49, 0x49, 0x36],
                '4' => &[0x18, 0x14, 0x12, 0x7F, 0x10],
                '5' => &[0x27, 0x45, 0x45, 0x45, 0x39],
                '6' => &[0x3C, 0x4A, 0x49, 0x49, 0x30],
                '7' => &[0x01, 0x71, 0x09, 0x05, 0x03],
                '8' => &[0x3E, 0x41, 0x49, 0x41, 0x3E],
                '9' => &[0x0E, 0x49, 0x49, 0x29, 0x1E],
                'A' => &[0x7E, 0x09, 0x09, 0x09, 0x7E],
                'B' => &[0x7F, 0x49, 0x49, 0x49, 0x36],
                'C' => &[0x3E, 0x41, 0x41, 0x41, 0x22],
                'D' => &[0x7F, 0x41, 0x41, 0x41, 0x3E],
                'E' => &[0x7F, 0x49, 0x49, 0x49, 0x41],
                'F' => &[0x7F, 0x09, 0x09, 0x09, 0x01],
                'G' => &[0x3E, 0x41, 0x49, 0x49, 0x7A],
                'H' => &[0x7F, 0x08, 0x08, 0x08, 0x7F],
                'I' => &[0x41, 0x41, 0x7F, 0x41, 0x41],
                'J' => &[0x70, 0x70, 0x70, 0x7F, 0x0F],
                'K' => &[0x7F, 0x08, 0x14, 0x22, 0x41],
                'L' => &[0x7F, 0x40, 0x40, 0x40, 0x40],
                'M' => &[0x7F, 0x02, 0x04, 0x02, 0x7F],
                'N' => &[0x7F, 0x02, 0x04, 0x08, 0x7F],
                'O' => &[0x3E, 0x41, 0x41, 0x41, 0x3E],
                'P' => &[0x7F, 0x09, 0x09, 0x09, 0x06],
                'Q' => &[0x3E, 0x41, 0x51, 0x61, 0x7E],
                'R' => &[0x7F, 0x09, 0x19, 0x29, 0x46],
                'S' => &[0x26, 0x49, 0x49, 0x49, 0x32],
                'T' => &[0x01, 0x01, 0x7F, 0x01, 0x01],
                'U' => &[0x3F, 0x40, 0x40, 0x40, 0x3F],
                'V' => &[0x1F, 0x20, 0x40, 0x20, 0x1F],
                'W' => &[0x7F, 0x20, 0x18, 0x20, 0x7F],
                'X' => &[0x63, 0x14, 0x08, 0x14, 0x63],
                'Y' => &[0x03, 0x04, 0x78, 0x04, 0x03],
                'Z' => &[0x41, 0x61, 0x51, 0x49, 0x43],
                _ => return None,
            })
        }
    }
}

///
/// ## Launch path (interim)
///
/// The supervisor has no launch interface yet (`init` only spawns its static
/// manifest), so `mimed` publishes a fire-and-forget
/// `system/events/open/<app>` event on `init`'s topic router with a
/// `path=<path> mime=<mime> verb=<verb>` payload. An app id is the program's
/// 8.3 stem in lowercase (`editor` is `EDITOR.ELF`), so the eventual launch
/// interface can spawn `APP.ELF <path>` from the same event. Until then the
/// event is the observable launch record: `messengerctl log` shows it.
pub mod mime {
    use alloc::string::String;
    use alloc::vec::Vec;

    use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

    use super::{errno, registry, Endpoint, Error, Result};

    /// The MIME service's registered name.
    pub const NAME: &str = "os.lazy.mimed";

    /// `os.lazy.mimed.v1` as an interim eight-byte ABI id (the pattern the
    /// other interim service interfaces use).
    pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.mime.");

    /// Methods of the MIME service.
    pub mod method {
        /// MIME type for a path, from the database.
        pub const GUESS: u32 = 1;
        /// App registered for a type and verb.
        pub const LOOKUP: u32 = 2;
        /// Verbs registered for a type.
        pub const VERBS: u32 = 3;
        /// Guess, resolve, and publish the launch event.
        pub const OPEN: u32 = 4;
        /// Add or replace an open-with registration.
        pub const REGISTER: u32 = 5;
    }

    /// Protocol TLV field ids.
    pub mod field {
        /// Path to guess.
        pub const PATH: u16 = 1;
        /// MIME type.
        pub const MIME: u16 = 2;
        /// Shell verb (`open`, `edit`, `reveal`, ...).
        pub const VERB: u16 = 3;
        /// App id.
        pub const APP: u16 = 4;
        /// One verb of a `Verbs` reply.
        pub const VERBS: u16 = 5;
        /// Lookup verdict (`1` = an app is registered).
        pub const FOUND: u16 = 6;
        /// Whether the launch event went out.
        pub const PUBLISHED: u16 = 7;
        /// Launch event topic.
        pub const TOPIC: u16 = 8;
        /// Structured error reply.
        pub const ERROR: u16 = 9;
        /// Whether `init` launched the resolved app (issue #158).
        pub const LAUNCHED: u16 = 10;
    }

    /// Type reported for a path the database has no entry for.
    pub const FALLBACK_MIME: &str = "application/octet-stream";

    /// Verb [`Client::open`] falls back to when the requested verb has no
    /// registration for the guessed type.
    pub const DEFAULT_VERB: &str = "open";

    /// One `Open` resolution: the app that will handle the file, its type, and
    /// the launch event.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct OpenResult {
        /// App id from the open-with registry.
        pub app: String,
        /// Type the path guessed to.
        pub mime: String,
        /// Topic the launch event was published on.
        pub topic: String,
        /// Whether the launch event went out.
        pub published: bool,
        /// Whether `init` accepted the launch request for the app (#158).
        pub launched: bool,
    }

    /// A header for a MIME parcel of `method`.
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

    /// Wrap an encoded body in a MIME parcel.
    fn parcel(method: u32, body: Encoder) -> Parcel {
        Parcel {
            header: header(method),
            body: body.finish(),
            ..Parcel::default()
        }
    }

    /// A `Guess(path)` request.
    pub fn guess_request(path: &str) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::PATH, path).map_err(Error::Parcel)?;
        Ok(parcel(method::GUESS, body))
    }

    /// A `Lookup(mime, verb)` request.
    pub fn lookup_request(mime: &str, verb: &str) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::MIME, mime).map_err(Error::Parcel)?;
        body.string(field::VERB, verb).map_err(Error::Parcel)?;
        Ok(parcel(method::LOOKUP, body))
    }

    /// A `Verbs(mime)` request.
    pub fn verbs_request(mime: &str) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::MIME, mime).map_err(Error::Parcel)?;
        Ok(parcel(method::VERBS, body))
    }

    /// An `Open(path, verb)` request.
    pub fn open_request(path: &str, verb: &str) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::PATH, path).map_err(Error::Parcel)?;
        body.string(field::VERB, verb).map_err(Error::Parcel)?;
        Ok(parcel(method::OPEN, body))
    }

    /// A `Register(mime, app, verb)` request (the registry takes the latest
    /// registration for each type and verb).
    pub fn register_request(mime: &str, app: &str, verb: &str) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::MIME, mime).map_err(Error::Parcel)?;
        body.string(field::APP, app).map_err(Error::Parcel)?;
        body.string(field::VERB, verb).map_err(Error::Parcel)?;
        Ok(parcel(method::REGISTER, body))
    }

    /// A `Guess` reply carrying the type.
    pub fn guess_reply(mime: &str) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::MIME, mime).map_err(Error::Parcel)?;
        Ok(parcel(method::GUESS, body))
    }

    /// A `Lookup` reply: `FOUND`, then the app when one is registered.
    pub fn lookup_reply(app: Option<&str>) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::FOUND, app.is_some() as u64)
            .map_err(Error::Parcel)?;
        if let Some(app) = app {
            body.string(field::APP, app).map_err(Error::Parcel)?;
        }
        Ok(parcel(method::LOOKUP, body))
    }

    /// A `Verbs` reply: one string field per verb.
    pub fn verbs_reply(verbs: &[String]) -> Result<Parcel> {
        let mut body = Encoder::new();
        for verb in verbs {
            body.string(field::VERBS, verb).map_err(Error::Parcel)?;
        }
        Ok(parcel(method::VERBS, body))
    }

    /// An `Open` reply describing the resolution and the launch event.
    pub fn open_reply(result: &OpenResult) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::APP, &result.app)
            .map_err(Error::Parcel)?;
        body.string(field::MIME, &result.mime)
            .map_err(Error::Parcel)?;
        body.string(field::TOPIC, &result.topic)
            .map_err(Error::Parcel)?;
        body.u64(field::PUBLISHED, result.published as u64)
            .map_err(Error::Parcel)?;
        body.u64(field::LAUNCHED, result.launched as u64)
            .map_err(Error::Parcel)?;
        Ok(parcel(method::OPEN, body))
    }

    /// An empty success reply (a `Register`).
    pub fn ok_reply(method: u32) -> Parcel {
        parcel(method, Encoder::new())
    }

    /// The service's error answer: errno-style code plus friendly text. The
    /// client turns the code back into [`Error::Mime`].
    pub fn error_reply(method: u32, error: Error) -> Parcel {
        let code = error.errno().map(|code| -code).unwrap_or(errno::EINVAL);
        let mut body = Encoder::new();
        // A structured error field cannot overflow a fresh encoder here.
        let _ = body.error(field::ERROR, code as u32, error.message());
        parcel(method, body)
    }

    /// The first structured error field, when the reply is a service failure.
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

    /// The first string field with `id`, or a malformed-request error.
    pub fn string_field(parcel: &Parcel, id: u16) -> Result<String> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind == Kind::String && field.id == id {
                return Ok(String::from(field.as_str().map_err(Error::Parcel)?));
            }
        }
        Err(Error::Errno(-errno::EINVAL))
    }

    /// The first string field with `id`, when present.
    fn optional_string(parcel: &Parcel, id: u16) -> Option<String> {
        string_field(parcel, id).ok()
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

    /// Every string field with `id`, in order.
    fn string_fields(parcel: &Parcel, id: u16) -> Vec<String> {
        let mut values = Vec::new();
        let mut decoder = Decoder::new(&parcel.body);
        while let Ok(Some(field)) = decoder.next() {
            if field.kind == Kind::String && field.id == id {
                if let Ok(text) = field.as_str() {
                    values.push(String::from(text));
                }
            }
        }
        values
    }

    /// Decode a `Guess` reply.
    pub fn decode_guess(parcel: &Parcel) -> Result<String> {
        string_field(parcel, field::MIME)
    }

    /// Decode a `Lookup` reply; `None` when no app is registered.
    pub fn decode_lookup(parcel: &Parcel) -> Result<Option<String>> {
        if u64_field(parcel, field::FOUND).unwrap_or(0) == 0 {
            return Ok(None);
        }
        optional_string(parcel, field::APP)
            .map(Some)
            .ok_or(Error::Errno(-errno::EINVAL))
    }

    /// Decode a `Verbs` reply.
    pub fn decode_verbs(parcel: &Parcel) -> Result<Vec<String>> {
        Ok(string_fields(parcel, field::VERBS))
    }

    /// Decode an `Open` reply.
    pub fn decode_open(parcel: &Parcel) -> Result<OpenResult> {
        Ok(OpenResult {
            app: string_field(parcel, field::APP)?,
            mime: optional_string(parcel, field::MIME).unwrap_or_default(),
            topic: optional_string(parcel, field::TOPIC).unwrap_or_default(),
            published: u64_field(parcel, field::PUBLISHED).unwrap_or(0) != 0,
            launched: u64_field(parcel, field::LAUNCHED).unwrap_or(0) != 0,
        })
    }

    /// A client of the `mimed` service.
    pub struct Client {
        endpoint: Endpoint,
    }

    impl Client {
        /// Resolve [`NAME`] and wrap the service endpoint.
        pub fn connect() -> Result<Client> {
            Ok(Client {
                endpoint: registry::resolve(NAME)?,
            })
        }

        /// Wrap an already-resolved endpoint.
        pub fn from_endpoint(endpoint: Endpoint) -> Client {
            Client { endpoint }
        }

        /// The underlying service endpoint (diagnostics).
        pub fn endpoint(&self) -> Endpoint {
            self.endpoint
        }

        /// Run one request as a blocking call and fail on a service error.
        fn call(&self, request: &Parcel) -> Result<Parcel> {
            let reply = self.endpoint.call(request, None)?;
            if let Some(code) = error_field(&reply)? {
                return Err(Error::Mime(code));
            }
            Ok(reply)
        }

        /// MIME type for `path`.
        pub fn guess(&self, path: &str) -> Result<String> {
            let reply = self.call(&guess_request(path)?)?;
            decode_guess(&reply)
        }

        /// App registered for `mime` and `verb`; `None` when none is.
        pub fn lookup(&self, mime: &str, verb: &str) -> Result<Option<String>> {
            let reply = self.call(&lookup_request(mime, verb)?)?;
            decode_lookup(&reply)
        }

        /// Verbs registered for `mime`, in registration order.
        pub fn verbs(&self, mime: &str) -> Result<Vec<String>> {
            let reply = self.call(&verbs_request(mime)?)?;
            decode_verbs(&reply)
        }

        /// Guess `path`, resolve the app for `verb`, and publish the launch
        /// event. [`OpenResult::published`] reports whether the event went out.
        pub fn open(&self, path: &str, verb: &str) -> Result<OpenResult> {
            let reply = self.call(&open_request(path, verb)?)?;
            decode_open(&reply)
        }

        /// Add or replace the app registered for `mime` and `verb`.
        pub fn register(&self, mime: &str, app: &str, verb: &str) -> Result<()> {
            self.call(&register_request(mime, app, verb)?)?;
            Ok(())
        }
    }

    /// Convenience: the MIME type for `path`, or [`FALLBACK_MIME`] when the
    /// service is unreachable.
    pub fn guess(path: &str) -> String {
        match Client::connect().and_then(|client| client.guess(path)) {
            Ok(mime) => mime,
            Err(_) => String::from(FALLBACK_MIME),
        }
    }

    /// Convenience: the app registered for `mime` and `verb`; `None` when none
    /// is registered or the service is unreachable.
    pub fn lookup(mime: &str, verb: &str) -> Option<String> {
        Client::connect().ok()?.lookup(mime, verb).ok()?
    }

    /// Convenience: the verbs registered for `mime` (empty when unreachable).
    pub fn verbs(mime: &str) -> Vec<String> {
        Client::connect()
            .and_then(|client| client.verbs(mime))
            .unwrap_or_default()
    }

    /// Convenience: connect and open.
    pub fn open(path: &str, verb: &str) -> Result<OpenResult> {
        Client::connect()?.open(path, verb)
    }

    /// Convenience: connect and register.
    pub fn register(mime: &str, app: &str, verb: &str) -> Result<()> {
        Client::connect()?.register(mime, app, verb)
    }
}
// ---------------------------------------------------------------------------
// clipboard: the per-session clipboard service (issue #115)
// ---------------------------------------------------------------------------

/// Client and wire shapes for `clipboardd`, the per-session clipboard service
/// (`docs/platform-plan.md` section 4.5, `docs/messenger.md` section 19).
///
/// An interaction is a typed offer plus a request:
///
/// * [`Client::copy`] (or the lower-level [`offer_request`]) publishes one or
///   more MIME payloads for the caller's session; the service answers with a
///   **token**;
/// * [`Client::paste`] / [`Client::paste_token`] request a payload by token
///   and MIME. A **lazy** offer sends only its MIME list; when a paste finally
///   happens the service calls the owner's [`method::SERIALIZE`] on the
///   endpoint registered under the offer's sink and forwards the bytes, so the
///   owning app materializes the data on demand;
/// * every offer announces itself on the retained per-session topic
///   `session/<id>/clipboard/changed`; [`Client::subscribe_changes`] attaches a
///   subscriber so paste UIs refresh without polling.
///
/// # Buffer handle
///
/// There is no userspace shared-buffer syscall yet (the kernel object and its
/// `SHARE_ONLY` rule live in `kernel/src/ipc/shared.rs`; `keyd` documents the
/// same gap), so a paste's [`BufferHandle`] currently carries the bytes inside
/// the reply parcel, bounded by the service's [`MAX_DATA`] on the eager path.
/// The wire shape is what a mapped `SHARE_ONLY` buffer will carry once the op
/// lands.
///
/// # Policy
///
/// `Offer` and `Request` parcels put the *pseudo-interface* ids
/// [`WRITE_INTERFACE`] (`os.lazy.clipboard.write.v1`) and [`READ_INTERFACE`]
/// (`os.lazy.clipboard.read.v1`) in their parcel header. The kernel's
/// `ipc::authorize` hook derives `(interface_id, method)` from that header on
/// every outbound call, so an ACL rule keyed on `clipboard.write` /
/// `clipboard.read` gates offering and pasting, and a denial is recorded in the
/// kernel audit ring before the service ever sees the parcel. On top of that
/// the service enforces the **session scope**: a token offered by session A is
/// refused (and logged) for session B.
pub mod clipboard {
    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;

    use libmessenger::{Decoder, Encoder, Field, Header, Kind, Parcel, VERSION};

    use super::{errno, registry, router, sys, Endpoint, Error, Result};

    /// The service's registered name.
    pub const NAME: &str = "os.lazy.clipboard";

    /// Control interface id (`os.lazy.clipboard.v1`, the interim eight-byte ABI
    /// id the other services use).
    pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.clip.");

    /// Policy pseudo-interface an `Offer` call carries:
    /// `fnv1a64("os.lazy.clipboard.write.v1")` (the topics convention).
    pub const WRITE_INTERFACE: u64 = fnv1a64("os.lazy.clipboard.write.v1");

    /// Policy pseudo-interface a `Request` call carries:
    /// `fnv1a64("os.lazy.clipboard.read.v1")`.
    pub const READ_INTERFACE: u64 = fnv1a64("os.lazy.clipboard.read.v1");

    /// Interface the owner of a lazy offer serves for [`method::SERIALIZE`].
    pub const OWNER_INTERFACE: u64 = u64::from_le_bytes(*b"os.owner");

    /// FNV-1a 64, the `tools/midlc` interface-id hash, so policy can key the
    /// two pseudo-interfaces on the same value the kernel checks.
    const fn fnv1a64(text: &str) -> u64 {
        let bytes = text.as_bytes();
        let mut hash = 0xCBF2_9CE4_8422_2325u64;
        let mut index = 0;
        while index < bytes.len() {
            hash = (hash ^ bytes[index] as u64).wrapping_mul(0x0000_0100_0000_01B3);
            index += 1;
        }
        hash
    }

    /// Methods. `OFFER` travels on [`WRITE_INTERFACE`], `REQUEST` on
    /// [`READ_INTERFACE`], `SERIALIZE` on [`OWNER_INTERFACE`]; `PING` and
    /// `CURRENT` are the control interface.
    pub mod method {
        /// Write: publish typed payloads for the caller's session.
        pub const OFFER: u32 = 1;
        /// Read: fetch a payload by token and MIME.
        pub const REQUEST: u32 = 1;
        /// Owner: serialize one MIME of an offer on demand (lazy transfer).
        pub const SERIALIZE: u32 = 1;
        /// Control: liveness probe.
        pub const PING: u32 = 2;
        /// Control: current-offer metadata, never content.
        pub const CURRENT: u32 = 3;
    }

    /// Protocol TLV field ids.
    pub mod field {
        /// Offer token.
        pub const TOKEN: u16 = 1;
        /// One MIME type.
        pub const MIME: u16 = 2;
        /// MIME type array.
        pub const MIMES: u16 = 3;
        /// Owner endpoint name for the lazy serialization callback.
        pub const SINK: u16 = 4;
        /// Inline `{MIME, BYTES}` payload records.
        pub const DATA: u16 = 5;
        /// Payload bytes.
        pub const BYTES: u16 = 6;
        /// Whether an offer is live (`Current` reply).
        pub const FOUND: u16 = 7;
        /// Session id an offer belongs to.
        pub const SESSION: u16 = 8;
        /// Human-readable owner label.
        pub const OWNER: u16 = 9;
        /// Whether the offer is lazy.
        pub const LAZY: u16 = 10;
        /// Tick the offer was made.
        pub const TICK: u16 = 11;
        /// One offer metadata record.
        pub const OFFER: u16 = 12;
        /// Structured error reply.
        pub const ERROR: u16 = 13;
    }

    /// Longest MIME string the service accepts.
    pub const MAX_MIME: usize = 64;
    /// Most MIME types in one offer.
    pub const MAX_MIMES: usize = 8;
    /// Largest inline payload the service keeps for one offer.
    pub const MAX_DATA: usize = 8 * 1024;
    /// Longest owner label or sink name.
    pub const MAX_TEXT: usize = 128;

    /// Metadata for one live offer: identity and MIME types, never content.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct OfferInfo {
        /// Offer token clients pass back in a `Request`.
        pub token: u64,
        /// Human-readable owner label supplied at offer time.
        pub owner: String,
        /// Session the offer belongs to (kernel-stamped).
        pub session: u64,
        /// MIME types the offer carries.
        pub mimes: Vec<String>,
        /// Whether the payload is materialized lazily by the owner.
        pub lazy: bool,
        /// Tick the offer was made.
        pub tick: u64,
    }

    /// The payload a `Request` yields. The kernel's `SHARE_ONLY` shared-buffer
    /// object is the future home of `bytes`; until the userspace mapping
    /// syscall lands the bytes ride in the reply parcel (see module docs).
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct BufferHandle {
        /// Token of the offer that was read.
        pub token: u64,
        /// MIME type that was read.
        pub mime: String,
        /// Whether the bytes came from the owner's `Serialize` callback.
        pub lazy: bool,
        /// The payload bytes.
        pub bytes: Vec<u8>,
    }

    /// A decoded `Offer` request (the service's view).
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct OfferRequest {
        /// Human-readable owner label.
        pub owner: String,
        /// Owner callback endpoint name for a lazy offer.
        pub sink: Option<String>,
        /// MIME types offered.
        pub mimes: Vec<String>,
        /// Inline payloads for an eager offer.
        pub data: Vec<(String, Vec<u8>)>,
    }

    /// The scoped retained topic a session's paste UIs watch
    /// (`docs/messenger.md` section 19).
    pub fn changes_topic(session: u64) -> String {
        format!("session/{session}/clipboard/changed")
    }

    /// A header for a clipboard parcel on `interface_id`.
    ///
    /// `ALLOW_NESTED` is required: every client resolves the same service
    /// endpoint, and a paste by one task can overlap an offer by another, so
    /// the kernel's per-channel cycle check would otherwise refuse the second
    /// call with `-EDEADLK`. The service answers each request before servicing
    /// the next and only calls out on a *different* channel (the owner's
    /// `Serialize`), so nesting cannot form a cycle here.
    fn header(interface_id: u64, method: u32) -> Header {
        Header {
            version: VERSION,
            flags: libmessenger::flags::ALLOW_NESTED,
            interface_id,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        }
    }

    /// Wrap an encoded body in a clipboard parcel.
    fn parcel(interface_id: u64, method: u32, body: Encoder) -> Parcel {
        Parcel {
            header: header(interface_id, method),
            body: body.finish(),
            ..Parcel::default()
        }
    }

    /// `Offer(owner, mime_types) -> token` for an eager offer: the payloads
    /// ride along and the service keeps one bounded copy.
    pub fn offer_request(owner: &str, offers: &[(&str, &[u8])]) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::OWNER, owner).map_err(Error::Parcel)?;
        let mut mimes = Encoder::new();
        let mut data = Encoder::new();
        for (mime, bytes) in offers {
            mimes.string(field::MIMES, mime).map_err(Error::Parcel)?;
            let mut record = Encoder::new();
            record.string(field::MIME, mime).map_err(Error::Parcel)?;
            record.bytes(field::BYTES, bytes).map_err(Error::Parcel)?;
            data.record(field::DATA, &record).map_err(Error::Parcel)?;
        }
        body.array(field::MIMES, &mimes).map_err(Error::Parcel)?;
        body.array(field::DATA, &data).map_err(Error::Parcel)?;
        Ok(parcel(WRITE_INTERFACE, method::OFFER, body))
    }

    /// `Offer` for a lazy offer: only the MIME list crosses the wire. `sink`
    /// names the registry entry where the owner serves [`method::SERIALIZE`]
    /// when a paste actually happens.
    pub fn offer_lazy_request(owner: &str, sink: &str, mimes: &[&str]) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.string(field::OWNER, owner).map_err(Error::Parcel)?;
        body.string(field::SINK, sink).map_err(Error::Parcel)?;
        let mut array = Encoder::new();
        for mime in mimes {
            array.string(field::MIMES, mime).map_err(Error::Parcel)?;
        }
        body.array(field::MIMES, &array).map_err(Error::Parcel)?;
        Ok(parcel(WRITE_INTERFACE, method::OFFER, body))
    }

    /// An `Offer` reply carrying the new token.
    pub fn token_reply(token: u64) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::TOKEN, token).map_err(Error::Parcel)?;
        Ok(parcel(WRITE_INTERFACE, method::OFFER, body))
    }

    /// `Request(token, mime)`; `token == 0` selects the newest offer in the
    /// caller's session that lists `mime`.
    pub fn request_request(token: u64, mime: &str) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::TOKEN, token).map_err(Error::Parcel)?;
        body.string(field::MIME, mime).map_err(Error::Parcel)?;
        Ok(parcel(READ_INTERFACE, method::REQUEST, body))
    }

    /// A `Request` reply carrying the payload (the `BufferHandle` shape).
    pub fn request_reply(handle: &BufferHandle) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::TOKEN, handle.token)
            .map_err(Error::Parcel)?;
        body.string(field::MIME, &handle.mime)
            .map_err(Error::Parcel)?;
        body.bool(field::LAZY, handle.lazy).map_err(Error::Parcel)?;
        body.bytes(field::BYTES, &handle.bytes)
            .map_err(Error::Parcel)?;
        Ok(parcel(READ_INTERFACE, method::REQUEST, body))
    }

    /// `Serialize(token, mime)`: the service calls this on a lazy offer's owner
    /// endpoint when a paste happens.
    pub fn serialize_request(token: u64, mime: &str) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::TOKEN, token).map_err(Error::Parcel)?;
        body.string(field::MIME, mime).map_err(Error::Parcel)?;
        Ok(parcel(OWNER_INTERFACE, method::SERIALIZE, body))
    }

    /// The owner's `Serialize` answer.
    pub fn serialize_reply(bytes: &[u8]) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.bytes(field::BYTES, bytes).map_err(Error::Parcel)?;
        Ok(parcel(OWNER_INTERFACE, method::SERIALIZE, body))
    }

    /// A `Ping` request.
    pub fn ping_request() -> Parcel {
        parcel(INTERFACE, method::PING, Encoder::new())
    }

    /// A `Current` request (offer metadata only; never content).
    pub fn current_request() -> Parcel {
        parcel(INTERFACE, method::CURRENT, Encoder::new())
    }

    /// An empty successful reply on `interface_id`/`method`.
    pub fn ok_reply(interface_id: u64, method: u32) -> Parcel {
        parcel(interface_id, method, Encoder::new())
    }

    /// Encode `info` into a `Current` reply; `None` when no offer is live.
    pub fn current_reply(info: Option<&OfferInfo>) -> Result<Parcel> {
        let mut body = Encoder::new();
        body.u64(field::FOUND, info.is_some() as u64)
            .map_err(Error::Parcel)?;
        if let Some(info) = info {
            body.record(field::OFFER, &info_body(info)?)
                .map_err(Error::Parcel)?;
        }
        Ok(parcel(INTERFACE, method::CURRENT, body))
    }

    /// Encode an offer's metadata as the retained `.../clipboard/changed`
    /// event payload: a parcel with the `OFFER` record, never content.
    pub fn changed_payload(info: &OfferInfo) -> Result<Vec<u8>> {
        let mut body = Encoder::new();
        body.record(field::OFFER, &info_body(info)?)
            .map_err(Error::Parcel)?;
        let mut bytes = Vec::new();
        parcel(INTERFACE, method::CURRENT, body)
            .encode(&mut bytes)
            .map_err(Error::Parcel)?;
        Ok(bytes)
    }

    /// The offer-metadata body shared by `Current` and the changed event.
    fn info_body(info: &OfferInfo) -> Result<Encoder> {
        let mut body = Encoder::new();
        body.u64(field::TOKEN, info.token).map_err(Error::Parcel)?;
        body.string(field::OWNER, &info.owner)
            .map_err(Error::Parcel)?;
        body.u64(field::SESSION, info.session)
            .map_err(Error::Parcel)?;
        body.bool(field::LAZY, info.lazy).map_err(Error::Parcel)?;
        body.u64(field::TICK, info.tick).map_err(Error::Parcel)?;
        let mut array = Encoder::new();
        for mime in &info.mimes {
            array.string(field::MIMES, mime).map_err(Error::Parcel)?;
        }
        body.array(field::MIMES, &array).map_err(Error::Parcel)?;
        Ok(body)
    }

    /// The service's error answer: errno-style code plus friendly text.
    pub fn error_reply(interface_id: u64, method: u32, error: Error) -> Parcel {
        let code = error.errno().map(|code| -code).unwrap_or(errno::EINVAL);
        let mut body = Encoder::new();
        // A structured error field cannot overflow a fresh encoder here.
        let _ = body.error(field::ERROR, code as u32, error.message());
        parcel(interface_id, method, body)
    }

    /// The first structured error field, when the reply is a service failure.
    fn error_field(parcel: &Parcel) -> Result<Option<i64>> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(item) = decoder.next().map_err(Error::Parcel)? {
            if item.kind == Kind::Error && item.id == field::ERROR {
                let (code, _message) = item.error_parts().map_err(Error::Parcel)?;
                return Ok(Some(code as i64));
            }
        }
        Ok(None)
    }

    /// Decode an `Offer` request into its owner, sink, MIME list and payloads.
    pub fn decode_offer(parcel: &Parcel) -> Result<OfferRequest> {
        let mut request = OfferRequest::default();
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(item) = decoder.next().map_err(Error::Parcel)? {
            match (item.kind, item.id) {
                (Kind::String, field::OWNER) => {
                    request.owner = String::from(item.as_str().map_err(Error::Parcel)?);
                }
                (Kind::String, field::SINK) => {
                    request.sink = Some(String::from(item.as_str().map_err(Error::Parcel)?));
                }
                (Kind::Array, field::MIMES) => {
                    let mut nested = item.nested(0).map_err(Error::Parcel)?;
                    while let Some(entry) = nested.next().map_err(Error::Parcel)? {
                        if entry.kind == Kind::String {
                            request
                                .mimes
                                .push(String::from(entry.as_str().map_err(Error::Parcel)?));
                        }
                    }
                }
                (Kind::Array, field::DATA) => {
                    let mut nested = item.nested(0).map_err(Error::Parcel)?;
                    while let Some(entry) = nested.next().map_err(Error::Parcel)? {
                        if entry.kind != Kind::Struct {
                            continue;
                        }
                        let mut record = entry.nested(0).map_err(Error::Parcel)?;
                        let mut mime = String::new();
                        let mut bytes = Vec::new();
                        while let Some(part) = record.next().map_err(Error::Parcel)? {
                            match (part.kind, part.id) {
                                (Kind::String, field::MIME) => {
                                    mime = String::from(part.as_str().map_err(Error::Parcel)?);
                                }
                                (Kind::Bytes, field::BYTES) => {
                                    bytes = part.as_bytes().to_vec();
                                }
                                _ => {}
                            }
                        }
                        request.data.push((mime, bytes));
                    }
                }
                _ => {}
            }
        }
        Ok(request)
    }

    /// Decode a `Request` (or `Serialize`) into `(token, mime)`.
    pub fn decode_request(parcel: &Parcel) -> Result<(u64, String)> {
        let mut token = 0u64;
        let mut mime = String::new();
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(item) = decoder.next().map_err(Error::Parcel)? {
            match (item.kind, item.id) {
                (Kind::U64, field::TOKEN) => token = item.as_u64().map_err(Error::Parcel)?,
                (Kind::String, field::MIME) => {
                    mime = String::from(item.as_str().map_err(Error::Parcel)?);
                }
                _ => {}
            }
        }
        Ok((token, mime))
    }

    /// Decode a `Serialize` into `(token, mime)`.
    pub fn decode_serialize(parcel: &Parcel) -> Result<(u64, String)> {
        decode_request(parcel)
    }

    /// Decode a `Request`/`Serialize` reply's payload bytes.
    pub fn decode_bytes(parcel: &Parcel) -> Result<Vec<u8>> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(item) = decoder.next().map_err(Error::Parcel)? {
            if item.kind == Kind::Bytes && item.id == field::BYTES {
                return Ok(item.as_bytes().to_vec());
            }
        }
        Err(Error::Errno(-errno::EINVAL))
    }

    /// Decode an `Offer` reply's token.
    pub fn decode_token(parcel: &Parcel) -> Result<u64> {
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(item) = decoder.next().map_err(Error::Parcel)? {
            if item.kind == Kind::U64 && item.id == field::TOKEN {
                return item.as_u64().map_err(Error::Parcel);
            }
        }
        Err(Error::Errno(-errno::EINVAL))
    }

    /// Decode a `Current` reply into the live offer's metadata.
    pub fn decode_current(parcel: &Parcel) -> Result<Option<OfferInfo>> {
        let mut found = false;
        let mut info = OfferInfo::default();
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(item) = decoder.next().map_err(Error::Parcel)? {
            match (item.kind, item.id) {
                (Kind::U64, field::FOUND) => found = item.as_u64().map_err(Error::Parcel)? != 0,
                (Kind::Struct, field::OFFER) => info = decode_info_record(item)?,
                _ => {}
            }
        }
        Ok(found.then_some(info))
    }

    /// Decode a changed-event payload (the bytes the topic broker carries)
    /// into the offer metadata.
    pub fn decode_changed(event: &router::Event) -> Result<OfferInfo> {
        let parcel = Parcel::decode(&event.payload).map_err(Error::Parcel)?;
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(item) = decoder.next().map_err(Error::Parcel)? {
            if item.kind == Kind::Struct && item.id == field::OFFER {
                return decode_info_record(item);
            }
        }
        Err(Error::Errno(-errno::EINVAL))
    }

    /// Decode one `OFFER` metadata record.
    fn decode_info_record(record: Field<'_>) -> Result<OfferInfo> {
        let mut info = OfferInfo::default();
        let mut nested = record.nested(0).map_err(Error::Parcel)?;
        while let Some(item) = nested.next().map_err(Error::Parcel)? {
            match (item.kind, item.id) {
                (Kind::U64, field::TOKEN) => info.token = item.as_u64().map_err(Error::Parcel)?,
                (Kind::String, field::OWNER) => {
                    info.owner = String::from(item.as_str().map_err(Error::Parcel)?);
                }
                (Kind::U64, field::SESSION) => {
                    info.session = item.as_u64().map_err(Error::Parcel)?;
                }
                (Kind::Bool, field::LAZY) => info.lazy = item.as_bool().map_err(Error::Parcel)?,
                (Kind::U64, field::TICK) => info.tick = item.as_u64().map_err(Error::Parcel)?,
                (Kind::Array, field::MIMES) => {
                    let mut mimes = item.nested(0).map_err(Error::Parcel)?;
                    while let Some(entry) = mimes.next().map_err(Error::Parcel)? {
                        if entry.kind == Kind::String {
                            info.mimes
                                .push(String::from(entry.as_str().map_err(Error::Parcel)?));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(info)
    }

    /// A client of the clipboard service.
    pub struct Client {
        endpoint: Endpoint,
        session: u64,
    }

    impl Client {
        /// Resolve [`NAME`] and read the caller's session from the kernel.
        pub fn connect() -> Result<Client> {
            Client::from_endpoint(registry::resolve(NAME)?)
        }

        /// Wrap an already-resolved endpoint.
        pub fn from_endpoint(endpoint: Endpoint) -> Result<Client> {
            let mut cred = sys::Cred::default();
            sys::cred_get(None, &mut cred).map_err(Error::Errno)?;
            Ok(Client {
                endpoint,
                session: cred.session,
            })
        }

        /// The underlying service endpoint (diagnostics).
        pub fn endpoint(&self) -> Endpoint {
            self.endpoint
        }

        /// The session id the client's requests are stamped with.
        pub fn session(&self) -> u64 {
            self.session
        }

        /// Run one request as a blocking call and fail on a service error
        /// reply.
        fn call(&self, request: &Parcel) -> Result<Parcel> {
            let reply = self.endpoint.call(request, None)?;
            if let Some(code) = error_field(&reply)? {
                return Err(Error::Errno(-code));
            }
            Ok(reply)
        }

        /// `Offer(owner, mime_types) -> token`: publish the typed payloads for
        /// this task's session (the eager path, bounded by the service's
        /// [`MAX_DATA`]). [`Client::offer_lazy`] is the on-demand variant.
        pub fn copy(&self, owner: &str, offers: &[(&str, &[u8])]) -> Result<u64> {
            let reply = self.call(&offer_request(owner, offers)?)?;
            decode_token(&reply)
        }

        /// `Offer(owner, mime_types) -> token` for a lazy offer: this task
        /// keeps the data and serves [`method::SERIALIZE`] on the endpoint it
        /// registers under `sink`.
        pub fn offer_lazy(&self, owner: &str, sink: &str, mimes: &[&str]) -> Result<u64> {
            let reply = self.call(&offer_lazy_request(owner, sink, mimes)?)?;
            decode_token(&reply)
        }

        /// Paste the newest offer of this session that carries `mime`;
        /// `Ok(None)` when no offer has it.
        pub fn paste(&self, mime: &str) -> Result<Option<Vec<u8>>> {
            match self.call(&request_request(0, mime)?) {
                Ok(reply) => Ok(Some(decode_bytes(&reply)?)),
                Err(Error::Errno(code)) if code == -errno::ENOENT => Ok(None),
                Err(error) => Err(error),
            }
        }

        /// Paste one exact offer by token; a foreign-session token is refused
        /// with `-EACCES` and audited by the service.
        pub fn paste_token(&self, token: u64, mime: &str) -> Result<Vec<u8>> {
            let reply = self.call(&request_request(token, mime)?)?;
            decode_bytes(&reply)
        }

        /// The current offer's metadata (never content).
        pub fn current(&self) -> Result<Option<OfferInfo>> {
            let reply = self.call(&current_request())?;
            decode_current(&reply)
        }

        /// Attach to this session's retained
        /// `session/<id>/clipboard/changed` topic.
        pub fn subscribe_changes(&self) -> Result<router::Subscriber> {
            router::Bus::connect(NAME)?.subscribe(&changes_topic(self.session))
        }

        /// Round-trip probe.
        pub fn ping(&self) -> Result<()> {
            self.call(&ping_request())?;
            Ok(())
        }
    }
}
