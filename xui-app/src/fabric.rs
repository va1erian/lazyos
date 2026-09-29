//! Read-only Messenger fabric client for `fabricmon`: the versioned
//! `FabricStats` snapshot (syscall 5 `stats`), the kernel name registry
//! (`list`), and the userspace topics broker (`messengerd`'s
//! `os.lazy.messenger.topics` service).
//!
//! The block layouts mirror `user/src/messenger/` (which mirrors the kernel):
//! [`FabricStats`] is stats ABI v3 in the fixed little-endian word stream, the
//! registry reply is a `libmessenger` parcel whose body carries one `ENTRY`
//! record per name, and the broker reply carries one `ENTRY` per topic.
//!
//! The app is unprivileged: the snapshot is global (no handle) and the
//! registry/broker paths are ordinary syscall-5 requests, so no capability is
//! needed for a monitor.

use libmessenger::{flags, Decoder, Encoder, Header, Kind, Parcel, VERSION};

use crate::sys::{self, msg_op, MsgArgs, MsgResult, REGISTRY_TARGET_SELF};

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

/// Registry TLV field ids, mirroring `kernel/src/ipc/registry.rs`.
mod registry_field {
    pub const NAME: u16 = 1;
    pub const INTERFACES: u16 = 2;
    pub const OBJECT: u16 = 5;
    pub const OWNER: u16 = 6;
    pub const LEASE_REMAINING: u16 = 7;
    pub const ENTRY: u16 = 8;
}

/// The registry interface id: the first eight bytes of `os.lazy.messenger.registry.v1`.
const REGISTRY_INTERFACE: u64 = u64::from_le_bytes(*b"os.lazy.");
/// Registry method `resolve`.
const REGISTRY_RESOLVE: u32 = 2;
/// Largest `list` reply the client offers the kernel (64 names of 128 bytes).
const REGISTRY_LIST_BUFFER: usize = 32 * 1024;

/// One registered name.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RegistryEntry {
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

/// Snapshot the kernel name table through syscall 5 `list`.
pub fn registry() -> Result<Vec<RegistryEntry>, i64> {
    let mut buf = vec![0u8; REGISTRY_LIST_BUFFER];
    let args = MsgArgs {
        txn_id: REGISTRY_TARGET_SELF,
        buf_ptr: buf.as_mut_ptr() as u64,
        buf_cap: buf.len() as u64,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    let code = sys::messenger(
        msg_op::LIST,
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
    let parcel = Parcel::decode(&buf[..len]).map_err(|_| -EINVAL)?;
    if let Some(error) = error_code(&parcel) {
        return Err(error);
    }
    decode_registry(&parcel).ok_or(-EINVAL)
}

/// Decode the registry `ENTRY` records of a `list` reply.
fn decode_registry(parcel: &Parcel) -> Option<Vec<RegistryEntry>> {
    let mut entries = Vec::new();
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(record)) = decoder.next() {
        if record.kind != Kind::Struct || record.id != registry_field::ENTRY {
            continue;
        }
        let mut nested = record.nested(0).ok()?;
        let mut entry = RegistryEntry {
            name: String::new(),
            object_id: 0,
            owner_slot: 0,
            interfaces: Vec::new(),
            lease_remaining: 0,
        };
        while let Ok(Some(item)) = nested.next() {
            match (item.kind, item.id) {
                (Kind::String, registry_field::NAME) => {
                    entry.name = item.as_str().ok()?.to_string()
                }
                (Kind::U64, registry_field::OBJECT) => entry.object_id = item.as_u64().ok()?,
                (Kind::U64, registry_field::OWNER) => entry.owner_slot = item.as_u64().ok()?,
                (Kind::U64, registry_field::LEASE_REMAINING) => {
                    entry.lease_remaining = item.as_u64().ok()?
                }
                (Kind::Array, registry_field::INTERFACES) => {
                    let mut array = item.nested(0).ok()?;
                    while let Ok(Some(id)) = array.next() {
                        if id.kind == Kind::U64 {
                            entry.interfaces.push(id.as_u64().ok()?);
                        }
                    }
                }
                _ => {}
            }
        }
        entries.push(entry);
    }
    Some(entries)
}

/// The topics-broker well-known name.
pub const TOPICS_NAME: &str = "os.lazy.messenger.topics";
/// Topics interface id: `fnv1a64("os.lazy.messenger.topics.v1")`.
const TOPICS_INTERFACE: u64 = 0xc573_4f97_8fef_7231;
/// Broker method `list_topics` (`fnv1a32` of the method name).
const TOPICS_LIST_TOPICS: u32 = 225_427_937;
/// Broker TLV field ids used here.
mod topics_field {
    pub const TOPIC: u16 = 1;
    pub const SUBSCRIBERS: u16 = 15;
    pub const ENTRY: u16 = 16;
}

/// Ticks a broker call waits before it gives up (`2 s` at 100 Hz), so a stalled
/// broker cannot freeze the dashboard forever.
const BROKER_CALL_TICKS: u64 = 200;

/// One topic the broker has seen, with its live subscriber count.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TopicRow {
    /// Topic name.
    pub topic: String,
    /// Live subscriptions whose filter matches it.
    pub subscribers: u64,
    /// Whether the broker holds a retained value for it.
    pub retained: bool,
}

/// Where the topics panel's data came from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Topics {
    /// The broker answered with its topic list.
    Online(Vec<TopicRow>),
    /// No broker is running (or it refused/failed); the negative errno.
    Offline(i64),
}

/// A cached topics-broker endpoint.
///
/// `CLOSE_ENDPOINT` marks an endpoint closed for every holder — including the
/// kernel's published service side — so a client that closes its resolved
/// handle after a call kills the channel the broker receives on. The monitor
/// therefore resolves the broker once and reuses the handle for every refresh;
/// only the kernel's own registry `resolve`/`list` paths are one-shot.
pub struct Broker {
    endpoint: Option<u64>,
}

impl Default for Broker {
    fn default() -> Self {
        Self::new()
    }
}

impl Broker {
    /// No endpoint resolved yet.
    pub const fn new() -> Broker {
        Broker { endpoint: None }
    }

    /// List topics through the userspace broker, resolving it on first use.
    pub fn topics(&mut self) -> Topics {
        let request = Parcel {
            header: Header {
                version: VERSION,
                flags: flags::SYNC | flags::ALLOW_NESTED,
                interface_id: TOPICS_INTERFACE,
                method: TOPICS_LIST_TOPICS,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body: Encoder::new().finish(),
            handles: Vec::new(),
            buffers: Vec::new(),
        };
        let mut buf = vec![0u8; 16 * 1024];
        let len = match self.call(&request, &mut buf) {
            Ok(len) => len,
            Err(code) => return Topics::Offline(code),
        };
        let parcel = match Parcel::decode(&buf[..len]) {
            Ok(parcel) => parcel,
            Err(_) => return Topics::Offline(-EINVAL),
        };
        if let Some(error) = error_code(&parcel) {
            return Topics::Offline(error);
        }
        match decode_topics(&parcel) {
            Some(topics) => Topics::Online(topics),
            None => Topics::Offline(-EINVAL),
        }
    }

    /// Run one request on the cached endpoint, resolving it first if needed.
    fn call(&mut self, request: &Parcel, buf: &mut [u8]) -> Result<usize, i64> {
        let endpoint = match self.endpoint {
            Some(endpoint) => endpoint,
            None => {
                let endpoint = resolve(TOPICS_NAME)?;
                self.endpoint = Some(endpoint);
                endpoint
            }
        };
        match call_on(endpoint, request, buf) {
            Ok(len) => Ok(len),
            Err(code) => {
                // A handle that names a dead endpoint (or no longer exists)
                // cannot recover; release the table slot and resolve fresh on
                // the next refresh. Transient failures (a timeout) keep it.
                if code == -ENOENT || code == -EPIPE {
                    self.endpoint = None;
                    let _ = close(endpoint);
                }
                Err(code)
            }
        }
    }
}

/// Resolve `name` into this task's handle table through syscall 5 `resolve`.
fn resolve(name: &str) -> Result<u64, i64> {
    let mut body = Encoder::new();
    body.string(registry_field::NAME, name)
        .map_err(|_| -EINVAL)?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: REGISTRY_INTERFACE,
            method: REGISTRY_RESOLVE,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|_| -EINVAL)?;
    let args = MsgArgs {
        txn_id: REGISTRY_TARGET_SELF,
        parcel_ptr: bytes.as_ptr() as u64,
        parcel_len: bytes.len() as u64,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    let code = sys::messenger(
        msg_op::RESOLVE,
        &args as *const MsgArgs as u64,
        &mut result as *mut MsgResult as u64,
    );
    if code < 0 {
        return Err(code);
    }
    Ok(result.value)
}

/// A blocking call with the broker's deadline; returns the reply length.
fn call_on(handle: u64, request: &Parcel, buf: &mut [u8]) -> Result<usize, i64> {
    let mut bytes = Vec::new();
    request.encode(&mut bytes).map_err(|_| -EINVAL)?;
    let args = MsgArgs {
        handle,
        parcel_ptr: bytes.as_ptr() as u64,
        parcel_len: bytes.len() as u64,
        buf_ptr: buf.as_mut_ptr() as u64,
        buf_cap: buf.len() as u64,
        deadline: sys::clock_ticks().saturating_add(BROKER_CALL_TICKS),
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    let code = sys::messenger(
        msg_op::CALL,
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
    Ok(len)
}

/// Close an endpoint handle opened by [`resolve`].
fn close(handle: u64) -> Result<(), i64> {
    let args = MsgArgs {
        handle,
        ..MsgArgs::default()
    };
    let code = sys::messenger(
        msg_op::CLOSE_ENDPOINT,
        &args as *const MsgArgs as u64,
        &mut MsgResult::default() as *mut MsgResult as u64,
    );
    if code < 0 {
        Err(code)
    } else {
        Ok(())
    }
}

/// Decode the broker's `ENTRY` topic records.
fn decode_topics(parcel: &Parcel) -> Option<Vec<TopicRow>> {
    let mut topics = Vec::new();
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(record)) = decoder.next() {
        if record.kind != Kind::Struct || record.id != topics_field::ENTRY {
            continue;
        }
        let mut nested = record.nested(0).ok()?;
        let mut row = TopicRow {
            topic: String::new(),
            subscribers: 0,
            retained: false,
        };
        while let Ok(Some(item)) = nested.next() {
            match (item.kind, item.id) {
                (Kind::String, topics_field::TOPIC) => row.topic = item.as_str().ok()?.to_string(),
                (Kind::U64, topics_field::SUBSCRIBERS) => row.subscribers = item.as_u64().ok()?,
                (Kind::Bool, _) => row.retained = item.as_bool().ok()?,
                _ => {}
            }
        }
        topics.push(row);
    }
    Some(topics)
}

/// The first structured `ERROR` field of a reply, when it carries one.
fn error_code(parcel: &Parcel) -> Option<i64> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::Error {
            return field.error_parts().ok().map(|(code, _)| code as i64);
        }
    }
    None
}

/// Errno values used here (Linux numbering; matches the native ABI).
const E2BIG: i64 = 7;
/// No such handle or name.
const ENOENT: i64 = 2;
/// The peer endpoint is gone.
const EPIPE: i64 = 32;
/// Invalid argument.
const EINVAL: i64 = 22;

/// A friendly one-line explanation of a negative errno returned by syscall 5.
pub fn errno_text(code: i64) -> String {
    let text = match -code {
        1 => "operation not permitted",
        2 => "not found",
        3 => "no such process",
        7 => "buffer too large",
        11 => "try again",
        12 => "out of memory",
        13 => "permission denied",
        14 => "bad address",
        22 => "invalid argument",
        32 => "peer closed",
        35 => "deadlock",
        110 => "timed out",
        125 => "canceled",
        _ => "unknown error",
    };
    format!("{} ({code})", text)
}
