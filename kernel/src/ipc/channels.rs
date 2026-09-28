//! Channels: duplex bounded queues and synchronous transactions (issue #66).
//!
//! This is the transport half of `docs/messenger.md` sections 6 and 14: one
//! channel has two endpoints ("sides"), each with a bounded inbox, plus a
//! transaction table that matches a `call` to the `reply` that answers it.
//!
//! ```text
//!   task A                     kernel                      task B
//!   handle side 0  --->  [ side 1 inbox ]  --->  handle side 1
//!                  <---  [ side 0 inbox ]  <---  reply(txn, parcel)
//! ```
//!
//! Endpoints are named by `HandleKind::Channel` handles whose `object_id` packs
//! `(channel_id << 1) | side`. The channel state itself lives in the `CHANNELS`
//! registry, so handle numbers stay small integers and a channel dies only when
//! both endpoints close.
//!
//! Blocking uses the generalized wait queues (issue #57) through one shared
//! `MESSENGER` queue. Wakeups are advisory: every event wakes all waiters and
//! each waiter re-checks its own inbox or transaction, so no per-channel queue
//! object is needed. `call` is `begin_call` (register + enqueue + park)
//! followed by `await_reply` (re-check until the outcome is terminal); the
//! split is also what asynchronous completion will build on in #69.
//!
//! Locking: `CHANNELS` is held only for registry mutations and is always
//! released before `MESSENGER` is notified, keeping the queue-then-task lock
//! order documented in `task::wait`. Transfer work (`shared`, handle table) is
//! done either before the channel lock is taken or after it is dropped, so the
//! only nesting is `CHANNELS` -> `REGISTRY`/handle table while a message is
//! queued with its references taken.
//!
//! # Handle and buffer transfer (issue #67)
//!
//! A parcel's `handles` list **moves** each handle: the sender's handle is
//! resolved and closed when the message is queued, and the receiver's table
//! gains a new handle when it takes delivery (`try_recv`/`recv` rewrites the
//! list to receiver-local numbers). A sender that still needs the handle must
//! duplicate it before sending. Every moved handle needs `TRANSFER` rights.
//!
//! A parcel's `buffers` list **shares**: each `BufferDesc` takes one message
//! reference to the buffer (the sender keeps its handle and mapping) and
//! delivery installs a receiver-local buffer handle without copying a byte.
//! The receiver maps it on demand with `ipc::shared::map`, which refuses
//! `SHARE_ONLY` buffers for anyone but the creator.
//!
//! Replies carry no transfers yet: a reply parcel with handles or buffers is
//! refused with [`Error::UnsupportedTransfer`].

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;

use libmessenger::{flags, BufferDesc, Parcel};

use crate::ipc::credentials;
use crate::ipc::handles::{self, rights, HandleKind};
use crate::ipc::shared;
use crate::quota::{self, Resource};
use crate::task::wait::WaitQueue;
use crate::task::{self, WaitKind, WakeReason};

/// Largest number of live channels.
pub const MAX_CHANNELS: usize = 64;
/// Largest number of messages parked in one endpoint's inbox.
pub const MAX_QUEUE_DEPTH: usize = 64;
/// Largest number of parcel bytes parked in one endpoint's inbox.
pub const MAX_QUEUE_BYTES: usize = 1 << 20;
/// Largest number of outstanding transactions on one channel.
pub const MAX_OUTSTANDING: usize = 64;
/// Largest number of outstanding transactions from one sender on one channel.
pub const MAX_PENDING_PER_SENDER: u64 = 16;

/// Global channel registry: small enough that a linear scan beats a map, and
/// exactly the "pairs of bounded queues" shape of section 14.
static CHANNELS: Mutex<Vec<Channel>> = Mutex::new(Vec::new());
/// Channel ids start at 1 so no handle ever carries object id 0.
static NEXT_CHANNEL_ID: AtomicU64 = AtomicU64::new(1);
/// Transaction ids are global; replies are matched by id alone.
static NEXT_TXN_ID: AtomicU64 = AtomicU64::new(1);

/// Wait queue for every Messenger blocking operation.
///
/// `WaitKind::Sleep` is the closest existing kind (this issue keeps `task/**`
/// frozen); adding a `WaitKind::Messenger` is a follow-up.
static MESSENGER: WaitQueue = WaitQueue::new(WaitKind::Sleep);

/// Test-only hooks for the kernel suite (issue #62 pattern). The channel tests
/// need to park a task exactly where `recv` parks it, without entering the
/// scheduler (the suite runs with interrupts disabled).
#[cfg(laZYOS_TESTS)]
pub mod harness {
    /// Park `slot` on the messenger wait queue without switching context.
    pub fn park(slot: usize, deadline: Option<u64>) {
        super::MESSENGER.park(slot, deadline);
    }
}

/// Why a channel operation failed. Messages are user-facing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The handle is unused or out of range.
    InvalidHandle,
    /// The handle exists but does not name a channel endpoint.
    WrongKind,
    /// The handle lacks the right the operation needs.
    MissingRight,
    /// The process is holding the maximum number of handles.
    NoFreeHandle,
    /// No task exists in the current slot.
    BadTask,
    /// The parcel is malformed or exceeds the Messenger size limits.
    BadParcel,
    /// The kernel channel registry is full.
    RegistryFull,
    /// The peer's inbox is at its depth or byte limit.
    QueueFull,
    /// The channel already has [`MAX_OUTSTANDING`] pending transactions.
    TooManyOutstanding,
    /// This sender already has [`MAX_PENDING_PER_SENDER`] pending transactions.
    Quota,
    /// The sender's *user* is over its per-uid queue quota (issue #103).
    QuotaExceeded,
    /// The call would form a synchronous cycle on this channel pair.
    Deadlock,
    /// No pending transaction has that id.
    NoTransaction,
    /// Only the task that started the transaction may cancel it.
    NotCaller,
    /// The deadline passed before a reply arrived.
    TimedOut,
    /// The caller canceled the transaction.
    Canceled,
    /// The endpoint on the other side was closed.
    PeerDied,
    /// A transferred handle is missing, of the wrong kind, or lacks the right
    /// to be moved.
    BadTransfer,
    /// Transfers are only carried by messages, not by replies (yet).
    UnsupportedTransfer,
}

impl Error {
    /// A short, human-readable explanation (friendly-errors convention).
    pub fn message(self) -> &'static str {
        match self {
            Error::InvalidHandle => "that Messenger handle does not exist",
            Error::WrongKind => "that handle does not name a channel endpoint",
            Error::MissingRight => "this handle does not grant the right to use the channel",
            Error::NoFreeHandle => "the process is holding too many Messenger handles",
            Error::BadTask => "no task exists in that slot",
            Error::BadParcel => "the parcel is malformed or exceeds a Messenger size limit",
            Error::RegistryFull => "the kernel channel registry is full",
            Error::QueueFull => "the peer's message queue is full",
            Error::TooManyOutstanding => "this channel has too many outstanding transactions",
            Error::Quota => "this sender has too many outstanding transactions",
            Error::QuotaExceeded => "this user is over its Messenger queue quota",
            Error::Deadlock => {
                "this call would deadlock: another transaction on this channel is still open"
            }
            Error::NoTransaction => "no transaction with that id is outstanding",
            Error::NotCaller => "only the task that started a transaction may cancel it",
            Error::TimedOut => "the deadline passed before a reply arrived",
            Error::Canceled => "the caller canceled this transaction",
            Error::PeerDied => "the endpoint on the other side was closed",
            Error::BadTransfer => {
                "a transferred handle does not exist or does not grant the transfer right"
            }
            Error::UnsupportedTransfer => "replies cannot carry handles or buffers yet",
        }
    }
}

/// Translate a handle-table error into the channel vocabulary.
fn from_handles(error: handles::Error) -> Error {
    match error {
        handles::Error::NoFreeHandle => Error::NoFreeHandle,
        handles::Error::InvalidHandle => Error::InvalidHandle,
        handles::Error::MissingRight => Error::MissingRight,
        handles::Error::BadTask => Error::BadTask,
        // A per-uid handle-quota refusal is the same user-facing condition as
        // the per-process cap (issue #103).
        handles::Error::Quota => Error::NoFreeHandle,
    }
}

/// Translate a shared-buffer error into the channel vocabulary.
fn from_shared(error: shared::Error) -> Error {
    match error {
        shared::Error::InvalidHandle | shared::Error::NotFound => Error::InvalidHandle,
        shared::Error::WrongKind => Error::WrongKind,
        shared::Error::MissingRight => Error::MissingRight,
        shared::Error::NoFreeHandle => Error::NoFreeHandle,
        shared::Error::BadTask => Error::BadTask,
        _ => Error::BadTransfer,
    }
}

/// A handle resolved out of the sender's table when a parcel is queued. The
/// number in the parcel is only meaningful to the sender; the kernel carries
/// the object identity instead, and the receiver gets a fresh local number.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Transfer {
    /// Kind of the object the handle names.
    pub kind: HandleKind,
    /// Rights the receiver's handle gets (equal to the sender's, which must
    /// include `TRANSFER`).
    pub rights: u32,
    /// Kernel object the handle names (opaque to userspace).
    pub object_id: u64,
}

/// A shared-buffer descriptor resolved out of the sender's table when a parcel
/// is queued. Unlike [`Transfer`], the sender keeps its own handle and mapping;
/// the message takes one reference and delivery installs a new handle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BufferTransfer {
    /// Buffer registry id.
    pub object_id: u64,
    /// Byte range of the buffer the message refers to.
    pub offset: u64,
    pub len: u64,
    /// Descriptor flags (metadata; the buffer's creation flags decide mapping).
    pub flags: u32,
    /// Rights the receiver's buffer handle gets.
    pub rights: u32,
}

/// One queued message: the encoded parcel plus the kernel-side metadata a
/// receiver needs to dispatch or answer it. Transfers are still unresolved
/// here: they become the receiver's handles in [`deliver`].
#[derive(Clone, PartialEq, Eq, Debug)]
struct Queued {
    sender: usize,
    /// Sender's uid *at queue time* (issue #103): the per-uid queue charge is
    /// released against this uid even if the sender transitions identity before
    /// the message is delivered.
    quota_uid: u32,
    method: u32,
    flags: u16,
    txn: Option<u64>,
    deadline: Option<u64>,
    bytes: Vec<u8>,
    handles: Vec<Transfer>,
    buffers: Vec<BufferTransfer>,
}

/// A delivered message: the encoded parcel header plus the handles and buffer
/// descriptors the receiver now owns, as numbers in the receiving task's
/// handle table. `bytes` still carries the sender's numbers (the wire form is
/// immutable); `handles` and `buffers` are the ones to use.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Message {
    /// Task slot that sent the message (kernel-stamped, never forgeable).
    pub sender: usize,
    /// Method id from the parcel header, copied out for dispatch.
    pub method: u32,
    /// Parcel header flags.
    pub flags: u16,
    /// The transaction to reply to for a request; `None` for one-way messages.
    pub txn: Option<u64>,
    /// Absolute PIT deadline of the transaction, for the callee's own timeout.
    pub deadline: Option<u64>,
    /// The encoded parcel, stored exactly as it was sent.
    pub bytes: Vec<u8>,
    /// Handles transferred by the sender, rewritten to local numbers.
    pub handles: Vec<u64>,
    /// Shared-buffer descriptors transferred by the sender, rewritten to
    /// local buffer handles. Map them on demand with `ipc::shared::map`.
    pub buffers: Vec<BufferDesc>,
}

/// Cumulative counters plus live depths, for `msg_stats` and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Synchronous calls started.
    pub calls: u64,
    /// Replies delivered.
    pub replies: u64,
    /// One-way messages accepted (sent minus calls, from the sender meters).
    pub one_way: u64,
    /// Transactions that hit their deadline.
    pub timeouts: u64,
    /// Transactions canceled by their caller.
    pub cancels: u64,
    /// Messages refused or discarded (full queue, dead peer, late reply).
    pub drops: u64,
    /// Messages currently queued on the channel.
    pub queued: u64,
    /// Parcel bytes currently queued on the channel.
    pub queued_bytes: u64,
    /// Transactions currently awaiting a reply.
    pub outstanding: u64,
}

/// Live registry sizes, for the fabric snapshot (issue #70).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Live channels in the registry.
    pub channels: u64,
    /// Live endpoints (two per channel until both sides close).
    pub endpoints: u64,
}

/// Per-sender metering on one channel (section 6's anti-flood accounting).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SenderMeter {
    /// Task slot of the sender.
    pub slot: usize,
    /// Messages (calls and one-way) this sender enqueued.
    pub sent: u64,
    /// Synchronous calls this sender started.
    pub calls: u64,
    /// Calls still awaiting a reply.
    pub outstanding: u64,
}

/// How a transaction ended. Only [`TxnState::Pending`] accepts a reply.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TxnState {
    Pending,
    Replied,
    TimedOut,
    Canceled,
    PeerDied,
}

/// One outstanding synchronous transaction.
struct Transaction {
    id: u64,
    /// Task slot waiting for the reply.
    caller: usize,
    /// Endpoint the caller used (the reply travels back to it).
    caller_side: usize,
    /// Endpoint the request was delivered to.
    callee_side: usize,
    deadline: Option<u64>,
    state: TxnState,
    /// Reply parcel bytes, valid while `state == Replied`.
    reply: Vec<u8>,
}

/// One side of a channel: a bounded inbox that messages are delivered into.
#[derive(Default)]
struct Endpoint {
    closed: bool,
    inbox: VecDeque<Queued>,
    queued_bytes: usize,
}

/// A duplex channel: two endpoints, their transactions, and their meters.
struct Channel {
    id: u64,
    endpoints: [Endpoint; 2],
    txns: Vec<Transaction>,
    senders: Vec<SenderMeter>,
    calls: u64,
    replies: u64,
    timeouts: u64,
    cancels: u64,
    drops: u64,
}

/// Pack the endpoint name into a handle's `object_id`.
fn object_id(channel_id: u64, side: usize) -> u64 {
    (channel_id << 1) | side as u64
}

/// Unpack a handle's `object_id` into `(channel_id, side)`.
fn split_object_id(object: u64) -> (u64, usize) {
    (object >> 1, (object & 1) as usize)
}

/// Find a channel in the registry, or report a stale handle.
fn find_channel(channels: &mut [Channel], id: u64) -> Result<&mut Channel, Error> {
    channels
        .iter_mut()
        .find(|channel| channel.id == id)
        .ok_or(Error::InvalidHandle)
}

/// Find a channel in the registry without mutating it.
fn find_channel_ref(channels: &[Channel], id: u64) -> Result<&Channel, Error> {
    channels
        .iter()
        .find(|channel| channel.id == id)
        .ok_or(Error::InvalidHandle)
}

/// Borrow the meter for `slot`, creating it on first use.
fn meter(channel: &mut Channel, slot: usize) -> &mut SenderMeter {
    if let Some(index) = channel.senders.iter().position(|meter| meter.slot == slot) {
        return &mut channel.senders[index];
    }
    channel.senders.push(SenderMeter {
        slot,
        sent: 0,
        calls: 0,
        outstanding: 0,
    });
    let last = channel.senders.len() - 1;
    &mut channel.senders[last]
}

/// A pending transaction just left `Pending`: one fewer call is in flight.
fn release_pending(channel: &mut Channel, caller: usize) {
    if let Some(meter) = channel
        .senders
        .iter_mut()
        .find(|meter| meter.slot == caller)
    {
        meter.outstanding = meter.outstanding.saturating_sub(1);
    }
}

/// Validate a parcel at the kernel boundary. Decoding is the attack surface;
/// this never panics and rejects anything over the wire limits.
fn validate_parcel(bytes: &[u8]) -> Result<Parcel, Error> {
    if bytes.len() > libmessenger::MAX_PARCEL_BYTES {
        return Err(Error::BadParcel);
    }
    Parcel::decode(bytes).map_err(|_| Error::BadParcel)
}

/// Resolve a parcel's `handles` and `buffers` against the sending task's
/// table, validating kinds, rights and the per-message limits. No reference is
/// taken here: [`retain_transfers`] runs once the message is accepted for
/// queueing, so a refused send changes nothing.
fn resolve_transfers(parcel: &Parcel) -> Result<(Vec<Transfer>, Vec<BufferTransfer>), Error> {
    if parcel.handles.len() > libmessenger::MAX_HANDLES
        || parcel.buffers.len() > libmessenger::MAX_BUFFERS
    {
        return Err(Error::BadParcel);
    }
    let mut transfers = Vec::with_capacity(parcel.handles.len());
    for (index, &local) in parcel.handles.iter().enumerate() {
        if parcel.handles[..index].contains(&local) {
            // One handle, one move: a duplicate entry would install two
            // receiver handles from a single reference.
            return Err(Error::BadTransfer);
        }
        let entry = handles::get(local).map_err(from_handles)?;
        if entry.rights & rights::TRANSFER == 0 {
            return Err(Error::MissingRight);
        }
        transfers.push(Transfer {
            kind: entry.kind,
            rights: entry.rights,
            object_id: entry.object_id,
        });
    }
    let mut buffers = Vec::with_capacity(parcel.buffers.len());
    for descriptor in &parcel.buffers {
        let entry = handles::get(descriptor.handle).map_err(from_handles)?;
        if entry.kind != HandleKind::Buffer {
            return Err(Error::WrongKind);
        }
        if entry.rights & rights::TRANSFER == 0 {
            return Err(Error::MissingRight);
        }
        buffers.push(BufferTransfer {
            object_id: entry.object_id,
            offset: descriptor.offset,
            len: descriptor.len,
            flags: descriptor.flags,
            rights: entry.rights,
        });
    }
    Ok((transfers, buffers))
}

/// Take one registry reference per buffer the message carries, releasing what
/// was already taken if a later entry fails.
fn retain_transfers(message: &Queued) -> Result<(), Error> {
    let mut retained: Vec<u64> = Vec::new();
    for transfer in &message.handles {
        if transfer.kind != HandleKind::Buffer {
            continue;
        }
        if let Err(error) = shared::retain(transfer.object_id) {
            for object_id in &retained {
                shared::release(*object_id);
            }
            return Err(from_shared(error));
        }
        retained.push(transfer.object_id);
    }
    for buffer in &message.buffers {
        if let Err(error) = shared::retain_descriptor(buffer.object_id, buffer.offset, buffer.len) {
            for object_id in &retained {
                shared::release(*object_id);
            }
            return Err(from_shared(error));
        }
        retained.push(buffer.object_id);
    }
    Ok(())
}

/// Charge one queued message to its sender's uid (issue #103): parcel bytes and
/// one queue slot, applied atomically so a refusal leaves nothing behind.
fn charge_queued(uid: u32, bytes: usize) -> Result<(), Error> {
    quota::charge_many(
        uid,
        &[
            (Resource::QueueBytes, bytes as u64),
            (Resource::QueueDepth, 1),
        ],
    )
    .map_err(|_| Error::QuotaExceeded)
}

/// Release the per-uid queue charge a message held, matched by
/// [`Queued::quota_uid`] so a delivery after a credential transition still
/// credits the user that was charged.
fn release_queued_quota(uid: u32, bytes: usize) {
    quota::release_many(
        uid,
        &[
            (Resource::QueueBytes, bytes as u64),
            (Resource::QueueDepth, 1),
        ],
    );
}

/// Release every reference a queued message that will never be delivered holds
/// (the receiving endpoint closed, the channel was dropped, or delivery failed
/// before the handles were installed).
fn release_queued(message: &Queued) {
    for transfer in &message.handles {
        if transfer.kind == HandleKind::Buffer {
            shared::release(transfer.object_id);
        }
    }
    for buffer in &message.buffers {
        shared::release(buffer.object_id);
    }
}

/// Finish a handle move: the sender's numbers were resolved into the message,
/// so close the sender's handles now that the message is safely queued.
fn close_moved_handles(numbers: &[u64], kinds: &[HandleKind]) {
    for (local, kind) in numbers.iter().zip(kinds.iter()) {
        if *kind == HandleKind::Buffer {
            shared::close(*local).ok();
        } else {
            handles::close(*local).ok();
        }
    }
}

/// Resolve a handle to `(channel_id, side)`, checking kind and rights.
fn endpoint_of(handle: u64, required: u32) -> Result<(u64, usize), Error> {
    let entry = handles::get(handle).map_err(from_handles)?;
    if entry.kind != HandleKind::Channel {
        return Err(Error::WrongKind);
    }
    if entry.rights & required != required {
        return Err(Error::MissingRight);
    }
    Ok(split_object_id(entry.object_id))
}

/// Create a channel and open both endpoint handles in the calling task.
///
/// Hand one endpoint to a peer by duplicating its handle and sending the copy
/// in a parcel's handle list (the transfer moves it; see the module docs).
/// Callers that drive both sides themselves (tests, bootstrap) can use the
/// pair directly.
pub fn create() -> Result<(u64, u64), Error> {
    let channel_id = NEXT_CHANNEL_ID.fetch_add(1, Ordering::Relaxed);
    {
        let mut channels = CHANNELS.lock();
        if channels.len() >= MAX_CHANNELS {
            return Err(Error::RegistryFull);
        }
        channels.push(Channel {
            id: channel_id,
            endpoints: [Endpoint::default(), Endpoint::default()],
            txns: Vec::new(),
            senders: Vec::new(),
            calls: 0,
            replies: 0,
            timeouts: 0,
            cancels: 0,
            drops: 0,
        });
    }
    let first = match handles::open(HandleKind::Channel, rights::ALL, object_id(channel_id, 0)) {
        Ok(handle) => handle,
        Err(error) => {
            CHANNELS.lock().retain(|channel| channel.id != channel_id);
            return Err(from_handles(error));
        }
    };
    match handles::open(HandleKind::Channel, rights::ALL, object_id(channel_id, 1)) {
        Ok(second) => Ok((first, second)),
        Err(error) => {
            handles::close(first).ok();
            CHANNELS.lock().retain(|channel| channel.id != channel_id);
            Err(from_handles(error))
        }
    }
}

/// Send a one-way message (section 7.1): enqueue and return immediately.
///
/// Ordering is preserved per sender to this channel and delivery is at most
/// once; a full queue is refused rather than blocking.
pub fn send(handle: u64, parcel_bytes: &[u8]) -> Result<(), Error> {
    let me = task::current();
    let (channel_id, side) = endpoint_of(handle, rights::CALL)?;
    let parcel = validate_parcel(parcel_bytes)?;
    let (handles, buffers) = resolve_transfers(&parcel)?;
    let numbers = parcel.handles.clone();
    let kinds: Vec<HandleKind> = handles.iter().map(|transfer| transfer.kind).collect();
    enqueue(
        channel_id,
        side,
        Queued {
            sender: me,
            quota_uid: credentials::of(me).uid,
            method: parcel.header.method,
            flags: parcel.header.flags,
            txn: None,
            deadline: None,
            bytes: parcel_bytes.to_vec(),
            handles,
            buffers,
        },
    )?;
    // The message owns the moved references now; the sender's numbers are gone.
    close_moved_handles(&numbers, &kinds);
    MESSENGER.notify_all();
    Ok(())
}

/// Enqueue a message into the peer endpoint's inbox, taking the buffer
/// references it carries and metering the sender.
fn enqueue(channel_id: u64, from_side: usize, message: Queued) -> Result<(), Error> {
    let peer = 1 - from_side;
    let mut channels = CHANNELS.lock();
    let channel = find_channel(&mut channels, channel_id)?;
    if channel.endpoints[peer].closed {
        return Err(Error::PeerDied);
    }
    let inbox = &channel.endpoints[peer];
    if inbox.inbox.len() >= MAX_QUEUE_DEPTH
        || inbox.queued_bytes.saturating_add(message.bytes.len()) > MAX_QUEUE_BYTES
    {
        channel.drops += 1;
        return Err(Error::QueueFull);
    }
    // Per-uid aggregate (issue #103): the endpoint's own depth/byte caps above
    // remain the first line, and the sender's user must also have room. Charge
    // before taking buffer references so a refusal has nothing to unwind.
    if let Err(error) = charge_queued(message.quota_uid, message.bytes.len()) {
        channel.drops += 1;
        return Err(error);
    }
    // The queue has room: take the message's buffer references so a sender
    // that closes its own handle cannot free frames an in-flight message needs.
    if let Err(error) = retain_transfers(&message) {
        release_queued_quota(message.quota_uid, message.bytes.len());
        channel.drops += 1;
        return Err(error);
    }
    let sender = message.sender;
    let bytes = message.bytes.len();
    let endpoint = &mut channel.endpoints[peer];
    endpoint.inbox.push_back(message);
    endpoint.queued_bytes += bytes;
    meter(channel, sender).sent += 1;
    Ok(())
}

/// Start a synchronous call (section 6): register a fresh `txn_id`, enqueue the
/// request, and park the caller until the transaction ends.
///
/// Returns the transaction id; the caller completes it with [`await_reply`].
/// A request that cannot even be queued (wrong handle, malformed parcel, full
/// queue, dead peer, nested cycle) fails before the caller parks.
pub fn begin_call(
    handle: u64,
    method: u32,
    parcel_bytes: &[u8],
    deadline: Option<u64>,
) -> Result<u64, Error> {
    let me = task::current();
    let (channel_id, side) = endpoint_of(handle, rights::CALL)?;
    let parcel = validate_parcel(parcel_bytes)?;
    let (handles, buffers) = resolve_transfers(&parcel)?;
    let numbers = parcel.handles.clone();
    let kinds: Vec<HandleKind> = handles.iter().map(|transfer| transfer.kind).collect();
    let peer = 1 - side;
    let txn_id = NEXT_TXN_ID.fetch_add(1, Ordering::Relaxed);
    {
        let mut channels = CHANNELS.lock();
        let channel = find_channel(&mut channels, channel_id)?;
        if channel.endpoints[peer].closed {
            return Err(Error::PeerDied);
        }
        if channel.endpoints[peer].inbox.len() >= MAX_QUEUE_DEPTH
            || channel.endpoints[peer]
                .queued_bytes
                .saturating_add(parcel_bytes.len())
                > MAX_QUEUE_BYTES
        {
            channel.drops += 1;
            return Err(Error::QueueFull);
        }
        let outstanding = channel
            .txns
            .iter()
            .filter(|txn| txn.state == TxnState::Pending)
            .count();
        if outstanding >= MAX_OUTSTANDING {
            channel.drops += 1;
            return Err(Error::TooManyOutstanding);
        }
        let sender_pending = channel
            .senders
            .iter()
            .find(|meter| meter.slot == me)
            .map(|meter| meter.outstanding)
            .unwrap_or(0);
        if sender_pending >= MAX_PENDING_PER_SENDER {
            channel.drops += 1;
            return Err(Error::Quota);
        }
        // Section 6: a synchronous call is a cycle, refused with ERR_DEADLOCK
        // unless ALLOW_NESTED opts out, when it is either
        // * nesting: this task already has a call open on the channel, or
        // * a callback: a call is open *toward* this side, so its caller is
        //   parked waiting for this side and cannot serve a call back.
        // Calls already open in the same direction by *other* tasks are not a
        // cycle: registry resolves alias one endpoint (`registry::resolve`),
        // so independent clients of a service share this channel and must be
        // able to call it concurrently; the service answers them in turn.
        let cycle = channel.txns.iter().any(|txn| {
            txn.state == TxnState::Pending && (txn.caller == me || txn.callee_side == side)
        });
        if cycle && parcel.header.flags & flags::ALLOW_NESTED == 0 {
            return Err(Error::Deadlock);
        }
        let queued = Queued {
            sender: me,
            quota_uid: credentials::of(me).uid,
            method,
            flags: parcel.header.flags,
            txn: Some(txn_id),
            deadline,
            bytes: parcel_bytes.to_vec(),
            handles,
            buffers,
        };
        // Per-uid queue quota (issue #103), then take the buffer references and
        // finish the handle move before the request is visible, so a callee that
        // runs immediately finds the transfers already installed in the message.
        if let Err(error) = charge_queued(queued.quota_uid, queued.bytes.len()) {
            channel.drops += 1;
            return Err(error);
        }
        if let Err(error) = retain_transfers(&queued) {
            release_queued_quota(queued.quota_uid, queued.bytes.len());
            channel.drops += 1;
            return Err(error);
        }
        close_moved_handles(&numbers, &kinds);
        channel.txns.push(Transaction {
            id: txn_id,
            caller: me,
            caller_side: side,
            callee_side: peer,
            deadline,
            state: TxnState::Pending,
            reply: Vec::new(),
        });
        let endpoint = &mut channel.endpoints[peer];
        endpoint.inbox.push_back(queued);
        endpoint.queued_bytes += parcel_bytes.len();
        channel.calls += 1;
        let sender = meter(channel, me);
        sender.sent += 1;
        sender.calls += 1;
        sender.outstanding += 1;
    }
    // Wake the callee: it may already be parked in `recv`, and nothing else
    // notifies the messenger queue for this request. `send` does the same; the
    // userspace `messengerd` round trip depends on it.
    MESSENGER.notify_all();
    // The request is visible now, so park before returning: syscalls run with
    // interrupts disabled, so no reply can slip in between registration and the
    // first wait and no wakeup can be lost.
    MESSENGER.park(me, deadline);
    Ok(txn_id)
}

/// Block until `txn_id` ends, then return the reply or the failure.
///
/// Every wakeup is treated as advisory: the transaction is re-checked under
/// the registry lock and the caller parks again if the outcome is not terminal
/// yet. A `TimedOut` wake marks the transaction expired, so a reply arriving
/// after the deadline is refused instead of delivered.
pub fn await_reply(txn_id: u64) -> Result<Vec<u8>, Error> {
    let me = task::current();
    loop {
        if let Some(outcome) = take_outcome(txn_id)? {
            // The terminal transition already woke us (or the event arrived
            // before the first wait); drop the reason so it cannot become a
            // spurious wakeup for the next blocking call.
            let _ = task::take_wake_reason(me);
            return outcome;
        }
        let deadline = transaction_deadline(txn_id)?;
        // `begin_call` already parked us; `wait` re-registers and the queue
        // cleans the duplicate entry when the wake is consumed.
        let reason = MESSENGER.wait(me, deadline);
        if reason == WakeReason::TimedOut {
            expire_transaction(txn_id);
        }
    }
}

/// Synchronous call convenience: [`begin_call`] plus [`await_reply`].
pub fn call(
    handle: u64,
    method: u32,
    parcel_bytes: &[u8],
    deadline: Option<u64>,
) -> Result<Vec<u8>, Error> {
    let txn_id = begin_call(handle, method, parcel_bytes, deadline)?;
    await_reply(txn_id)
}

/// Answer a pending transaction with a reply parcel.
///
/// Replies may arrive out of order (they match by id), and a reply to an
/// expired, canceled, or dead transaction is refused and counted as a drop.
/// Only the receiving endpoint's holder should call this; signing the reply
/// with the callee handle belongs to the syscall edge (#69).
pub fn reply(txn_id: u64, parcel_bytes: &[u8]) -> Result<(), Error> {
    let parcel = validate_parcel(parcel_bytes)?;
    // Replies travel back through `await_reply`, which returns bytes only;
    // installing reply-borne handles would need the caller's table at consume
    // time, so replies refuse transfers until that path grows one.
    if !parcel.handles.is_empty() || !parcel.buffers.is_empty() {
        return Err(Error::UnsupportedTransfer);
    }
    let mut found = false;
    {
        let mut channels = CHANNELS.lock();
        for channel in channels.iter_mut() {
            let Some(index) = channel.txns.iter().position(|txn| txn.id == txn_id) else {
                continue;
            };
            if channel.txns[index].state != TxnState::Pending {
                channel.drops += 1;
                return Err(Error::NoTransaction);
            }
            channel.txns[index].state = TxnState::Replied;
            channel.txns[index].reply = parcel_bytes.to_vec();
            channel.replies += 1;
            let caller = channel.txns[index].caller;
            release_pending(channel, caller);
            found = true;
            break;
        }
    }
    if !found {
        return Err(Error::NoTransaction);
    }
    MESSENGER.notify_all();
    Ok(())
}

/// Cancel a pending transaction. Only its caller may cancel; the wait in
/// [`await_reply`] ends with [`Error::Canceled`].
pub fn cancel(txn_id: u64) -> Result<(), Error> {
    let me = task::current();
    let mut found = false;
    {
        let mut channels = CHANNELS.lock();
        for channel in channels.iter_mut() {
            let Some(index) = channel.txns.iter().position(|txn| txn.id == txn_id) else {
                continue;
            };
            if channel.txns[index].caller != me {
                return Err(Error::NotCaller);
            }
            if channel.txns[index].state != TxnState::Pending {
                return Err(Error::NoTransaction);
            }
            channel.txns[index].state = TxnState::Canceled;
            channel.cancels += 1;
            let caller = channel.txns[index].caller;
            release_pending(channel, caller);
            found = true;
            break;
        }
    }
    if !found {
        return Err(Error::NoTransaction);
    }
    MESSENGER.notify_all();
    Ok(())
}

/// Close one endpoint: drop its handle, mark the side closed, and fail every
/// transaction that still needs it with `PeerDied` (section 9's peer death).
///
/// Messages already queued for the surviving side stay deliverable; once that
/// inbox drains, `recv`/`try_recv` report `PeerDied` too.
pub fn close_endpoint(handle: u64) -> Result<(), Error> {
    let (channel_id, side) = endpoint_of(handle, 0)?;
    handles::close(handle).map_err(from_handles)?;
    let mut remove = false;
    {
        let mut channels = CHANNELS.lock();
        if let Some(index) = channels.iter().position(|channel| channel.id == channel_id) {
            let channel = &mut channels[index];
            channel.endpoints[side].closed = true;
            // Anything still queued for the dead side will never be received;
            // release the buffer references those messages hold.
            channel.drops += channel.endpoints[side].inbox.len() as u64;
            let dropped: Vec<Queued> = channel.endpoints[side].inbox.drain(..).collect();
            channel.endpoints[side].queued_bytes = 0;
            for message in &dropped {
                release_queued(message);
                release_queued_quota(message.quota_uid, message.bytes.len());
            }
            let mut released = Vec::new();
            for txn in channel.txns.iter_mut() {
                if txn.state == TxnState::Pending
                    && (txn.caller_side == side || txn.callee_side == side)
                {
                    txn.state = TxnState::PeerDied;
                    released.push(txn.caller);
                }
            }
            for caller in released {
                release_pending(channel, caller);
            }
            remove = channel.endpoints[0].closed && channel.endpoints[1].closed;
            if remove {
                // The last side closed: undelivered messages for the surviving
                // side go away with the channel.
                let mut extra_drops = 0u64;
                let mut pending: Vec<Queued> = Vec::new();
                for endpoint in channel.endpoints.iter_mut() {
                    extra_drops += endpoint.inbox.len() as u64;
                    pending.extend(endpoint.inbox.drain(..));
                    endpoint.queued_bytes = 0;
                }
                channel.drops += extra_drops;
                for message in &pending {
                    release_queued(message);
                    release_queued_quota(message.quota_uid, message.bytes.len());
                }
            }
        }
        if remove {
            channels.retain(|channel| channel.id != channel_id);
        }
    }
    MESSENGER.notify_all();
    Ok(())
}

/// Receive the next message without blocking; `Ok(None)` means "try later".
///
/// Delivery installs the message's transferred handles and buffers into the
/// calling task's handle table and rewrites them to local numbers, so the
/// returned [`Message`] is immediately usable.
pub fn try_recv(handle: u64) -> Result<Option<Message>, Error> {
    let (channel_id, side) = endpoint_of(handle, rights::CALL)?;
    let queued = {
        let mut channels = CHANNELS.lock();
        let channel = find_channel(&mut channels, channel_id)?;
        let endpoint = &mut channel.endpoints[side];
        if let Some(message) = endpoint.inbox.pop_front() {
            endpoint.queued_bytes = endpoint.queued_bytes.saturating_sub(message.bytes.len());
            Some(message)
        } else if channel.endpoints[1 - side].closed {
            return Err(Error::PeerDied);
        } else {
            None
        }
    };
    match queued {
        Some(message) => {
            // Delivery takes the message out of the inbox, so the sender's
            // user gets the queue charge back (issue #103).
            release_queued_quota(message.quota_uid, message.bytes.len());
            Ok(Some(deliver(message)?))
        }
        None => Ok(None),
    }
}

/// Install a queued message's transfers into the receiving task's handle table
/// and rewrite them to local numbers.
///
/// On failure (the receiver is out of handles) everything installed is rolled
/// back and the references of everything still pending are released, so a
/// failed delivery cannot leak handles or frames.
fn deliver(queued: Queued) -> Result<Message, Error> {
    let mut handles_out: Vec<u64> = Vec::with_capacity(queued.handles.len());
    let mut buffers_out: Vec<BufferDesc> = Vec::with_capacity(queued.buffers.len());
    for (index, transfer) in queued.handles.iter().enumerate() {
        let opened = if transfer.kind == HandleKind::Buffer {
            shared::attach(transfer.object_id, transfer.rights).map_err(from_shared)
        } else {
            handles::open(transfer.kind, transfer.rights, transfer.object_id).map_err(from_handles)
        };
        match opened {
            Ok(handle) => handles_out.push(handle),
            Err(error) => {
                rollback_delivery(&queued, index, &handles_out, &buffers_out);
                return Err(error);
            }
        }
    }
    for buffer in queued.buffers.iter() {
        match shared::attach(buffer.object_id, buffer.rights) {
            Ok(handle) => buffers_out.push(BufferDesc {
                handle,
                offset: buffer.offset,
                len: buffer.len,
                flags: buffer.flags,
            }),
            Err(error) => {
                rollback_delivery(&queued, queued.handles.len(), &handles_out, &buffers_out);
                return Err(from_shared(error));
            }
        }
    }
    Ok(Message {
        sender: queued.sender,
        method: queued.method,
        flags: queued.flags,
        txn: queued.txn,
        deadline: queued.deadline,
        bytes: queued.bytes,
        handles: handles_out,
        buffers: buffers_out,
    })
}

/// Undo a partial [`deliver`]: close the installed handles and release the
/// message references of everything still pending.
///
/// Non-buffer objects have no kernel object refcount yet, so releasing a moved
/// handle whose delivery failed drops the handle but not the object; that is
/// the documented follow-up for when `HandleEntry` grows a refcount.
fn rollback_delivery(
    queued: &Queued,
    installed: usize,
    handles_out: &[u64],
    buffers_out: &[BufferDesc],
) {
    for (transfer, &handle) in queued.handles[..installed].iter().zip(handles_out) {
        if transfer.kind == HandleKind::Buffer {
            shared::close(handle).ok();
        } else {
            handles::close(handle).ok();
        }
    }
    for transfer in &queued.handles[installed..] {
        if transfer.kind == HandleKind::Buffer {
            shared::release(transfer.object_id);
        }
    }
    for descriptor in buffers_out {
        // Closing the receiver's buffer handle drops the reference `attach`
        // converted from the message.
        shared::close(descriptor.handle).ok();
    }
    for buffer in &queued.buffers[buffers_out.len()..] {
        shared::release(buffer.object_id);
    }
}

/// Receive the next message, parking until one arrives, the deadline passes, or
/// the peer closes.
pub fn recv(handle: u64, deadline: Option<u64>) -> Result<Message, Error> {
    loop {
        match try_recv(handle) {
            Ok(Some(message)) => return Ok(message),
            Ok(None) => {}
            Err(error) => return Err(error),
        }
        let reason = MESSENGER.wait(task::current(), deadline);
        if reason == WakeReason::TimedOut {
            return Err(Error::TimedOut);
        }
    }
}

/// Sum a channel's counters and live depths into `stats`.
fn accumulate(stats: &mut Stats, channel: &Channel) {
    stats.calls += channel.calls;
    stats.replies += channel.replies;
    stats.timeouts += channel.timeouts;
    stats.cancels += channel.cancels;
    stats.drops += channel.drops;
    // Every accepted message is metered as `sent`; a synchronous call also
    // meters `calls`, so the remainder is exactly the one-way traffic.
    stats.one_way += channel
        .senders
        .iter()
        .map(|meter| meter.sent.saturating_sub(meter.calls))
        .sum::<u64>();
    for endpoint in &channel.endpoints {
        stats.queued += endpoint.inbox.len() as u64;
        stats.queued_bytes += endpoint.queued_bytes as u64;
    }
    stats.outstanding += channel
        .txns
        .iter()
        .filter(|txn| txn.state == TxnState::Pending)
        .count() as u64;
}

/// Live channel and endpoint counts for the fabric snapshot (issue #70).
pub fn counts() -> Counts {
    let channels = CHANNELS.lock();
    Counts {
        channels: channels.len() as u64,
        endpoints: channels.len() as u64 * 2,
    }
}

/// Aggregated counters and depths across every live channel.
pub fn stats() -> Stats {
    let channels = CHANNELS.lock();
    let mut stats = Stats::default();
    for channel in channels.iter() {
        accumulate(&mut stats, channel);
    }
    stats
}

/// Counters and depths for the channel `handle` names.
pub fn channel_stats(handle: u64) -> Result<Stats, Error> {
    let (channel_id, _) = endpoint_of(handle, rights::CALL)?;
    let channels = CHANNELS.lock();
    let channel = find_channel_ref(&channels, channel_id)?;
    let mut stats = Stats::default();
    accumulate(&mut stats, channel);
    Ok(stats)
}

/// Per-sender metering for the channel `handle` names.
pub fn senders(handle: u64) -> Result<Vec<SenderMeter>, Error> {
    let (channel_id, _) = endpoint_of(handle, rights::CALL)?;
    let channels = CHANNELS.lock();
    let channel = find_channel_ref(&channels, channel_id)?;
    Ok(channel.senders.clone())
}

/// Drop every channel (process teardown, reboot, test isolation), releasing
/// the buffer references held by undelivered messages.
///
/// Waiters are woken so a task parked in `await_reply` observes
/// [`Error::NoTransaction`] instead of hanging.
pub fn reset() {
    let mut channels = CHANNELS.lock();
    for channel in channels.iter() {
        for endpoint in &channel.endpoints {
            for message in &endpoint.inbox {
                release_queued(message);
                release_queued_quota(message.quota_uid, message.bytes.len());
            }
        }
    }
    channels.clear();
    drop(channels);
    MESSENGER.notify_all();
}

/// The deadline recorded for a transaction, if it is still outstanding.
fn transaction_deadline(txn_id: u64) -> Result<Option<u64>, Error> {
    let channels = CHANNELS.lock();
    for channel in channels.iter() {
        if let Some(txn) = channel.txns.iter().find(|txn| txn.id == txn_id) {
            return Ok(txn.deadline);
        }
    }
    Err(Error::NoTransaction)
}

/// Remove and return a terminal transaction's outcome, or `Ok(None)` while it
/// is still pending.
fn take_outcome(txn_id: u64) -> Result<Option<Result<Vec<u8>, Error>>, Error> {
    let mut channels = CHANNELS.lock();
    for channel in channels.iter_mut() {
        let Some(index) = channel.txns.iter().position(|txn| txn.id == txn_id) else {
            continue;
        };
        if channel.txns[index].state == TxnState::Pending {
            return Ok(None);
        }
        let txn = channel.txns.remove(index);
        let outcome = match txn.state {
            TxnState::Replied => Ok(txn.reply),
            TxnState::TimedOut => Err(Error::TimedOut),
            TxnState::Canceled => Err(Error::Canceled),
            TxnState::PeerDied => Err(Error::PeerDied),
            TxnState::Pending => Err(Error::NoTransaction),
        };
        return Ok(Some(outcome));
    }
    Err(Error::NoTransaction)
}

/// Mark a pending transaction expired. A no-op if it already has another
/// terminal outcome, so a reply racing the deadline wins.
fn expire_transaction(txn_id: u64) {
    let mut channels = CHANNELS.lock();
    for channel in channels.iter_mut() {
        let Some(index) = channel.txns.iter().position(|txn| txn.id == txn_id) else {
            continue;
        };
        if channel.txns[index].state != TxnState::Pending {
            return;
        }
        channel.txns[index].state = TxnState::TimedOut;
        channel.timeouts += 1;
        let caller = channel.txns[index].caller;
        release_pending(channel, caller);
        return;
    }
}

/// Test hook (issue #62 harness): run the timer's deadline sweep and mark every
/// expired transaction `TimedOut`, exactly as the wait loop would after
/// `WaitQueue::wait` returned `WakeReason::TimedOut`. Compiled only for the
/// in-kernel suite.
#[cfg(laZYOS_TESTS)]
pub fn expire_deadlines(now: u64) {
    task::harness::expire_deadlines(now);
    let mut channels = CHANNELS.lock();
    for channel in channels.iter_mut() {
        let mut released = Vec::new();
        for index in 0..channel.txns.len() {
            let txn = &mut channel.txns[index];
            if txn.state == TxnState::Pending
                && txn.deadline.is_some_and(|deadline| deadline <= now)
            {
                txn.state = TxnState::TimedOut;
                released.push(txn.caller);
            }
        }
        channel.timeouts += released.len() as u64;
        for caller in released {
            release_pending(channel, caller);
        }
    }
    drop(channels);
    MESSENGER.notify_all();
}
