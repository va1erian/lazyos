//! Channel vocabulary: errors, transfers, messages, stats and the channel tables.

use super::*;

/// Translate a handle-table error into the channel vocabulary.
pub(super) fn from_handles(error: handles::Error) -> Error {
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
pub(super) fn from_shared(error: shared::Error) -> Error {
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
pub(super) struct Queued {
    pub(super) sender: usize,
    /// Sender's uid *at queue time* (issue #103): the per-uid queue charge is
    /// released against this uid even if the sender transitions identity before
    /// the message is delivered.
    pub(super) quota_uid: u32,
    pub(super) method: u32,
    pub(super) flags: u16,
    pub(super) txn: Option<u64>,
    pub(super) deadline: Option<u64>,
    pub(super) bytes: Vec<u8>,
    pub(super) handles: Vec<Transfer>,
    pub(super) buffers: Vec<BufferTransfer>,
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
pub(super) enum TxnState {
    Pending,
    Replied,
    TimedOut,
    Canceled,
    PeerDied,
}

/// One outstanding synchronous transaction.
pub(super) struct Transaction {
    pub(super) id: u64,
    /// Task slot waiting for the reply.
    pub(super) caller: usize,
    /// Endpoint the caller used (the reply travels back to it).
    pub(super) caller_side: usize,
    /// Endpoint the request was delivered to.
    pub(super) callee_side: usize,
    pub(super) deadline: Option<u64>,
    pub(super) state: TxnState,
    /// Reply parcel bytes, valid while `state == Replied`.
    pub(super) reply: Vec<u8>,
}

/// Task slots parked in `recv` on one endpoint, as a bitset: registering,
/// taking the set for a wake and unregistering never allocate (P6.3).
#[derive(Clone, Copy, Default)]
pub(super) struct WaiterSet([u64; crate::task::slotmask::WORDS]);

impl WaiterSet {
    pub(super) fn insert(&mut self, slot: usize) {
        if let Some(word) = self.0.get_mut(slot / 64) {
            *word |= 1 << (slot % 64);
        }
    }

    pub(super) fn remove(&mut self, slot: usize) {
        if let Some(word) = self.0.get_mut(slot / 64) {
            *word &= !(1 << (slot % 64));
        }
    }

    /// Every registered slot, leaving the set empty.
    pub(super) fn take(&mut self) -> WaiterSet {
        core::mem::take(self)
    }

    pub(super) fn len(&self) -> usize {
        self.0.iter().map(|word| word.count_ones() as usize).sum()
    }

    /// The registered slots, ascending.
    pub(super) fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        (0..crate::task::MAX_TASKS).filter(|slot| self.0[slot / 64] & (1 << (slot % 64)) != 0)
    }
}

/// One side of a channel: a bounded inbox that messages are delivered into.
#[derive(Default)]
pub(super) struct Endpoint {
    pub(super) closed: bool,
    pub(super) inbox: VecDeque<Queued>,
    pub(super) queued_bytes: usize,
    /// Task slots parked in `recv` on this side (issue #338): a delivery or
    /// a close wakes exactly these instead of every Messenger waiter. Each
    /// park registers under the registry lock, a wake takes the whole list,
    /// and a waiter that returns (message, timeout, error) unregisters, so
    /// the list only ever holds tasks currently inside `recv`.
    pub(super) waiters: WaiterSet,
    /// Poll transactions (see [`POLL_DEADLINE`]) this side has received and
    /// not yet finished serving: the receiver's next `recv` on this side
    /// ends any that are still unanswered with `TimedOut`.
    pub(super) serving_polls: Vec<u64>,
}

/// A duplex channel: two endpoints, their transactions, and their meters.
pub(super) struct Channel {
    pub(super) id: u64,
    pub(super) endpoints: [Endpoint; 2],
    pub(super) txns: Vec<Transaction>,
    pub(super) senders: Vec<SenderMeter>,
    pub(super) calls: u64,
    pub(super) replies: u64,
    pub(super) timeouts: u64,
    pub(super) cancels: u64,
    pub(super) drops: u64,
}
