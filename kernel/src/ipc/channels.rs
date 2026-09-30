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
//! `MESSENGER` queue, but wakeups are targeted (issue #338): each endpoint
//! records the tasks parked in `recv` on it, a delivery or close wakes only
//! those, and a reply/cancel/peer death wakes only the transaction's caller.
//! Wakeups stay advisory: a woken task re-checks its own inbox or
//! transaction, so no per-channel queue object is needed. `call` is `begin_call` (register + enqueue; the
//! caller stays runnable) followed by `await_reply` (park and re-check until
//! the outcome is terminal); the
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

// Split out to keep this file from growing (issue #194): the error type, and
// the kernel-originated posts the device core uses (issue #240).
#[path = "channels_error.rs"]
mod error;
pub use error::Error;
#[path = "channels_kernel.rs"]
mod kernel_post;
pub use kernel_post::{post_from_kernel, private_endpoint_of_task, seal_endpoint};

mod close;
mod recv;
mod stats;
mod support;
mod txn;
mod types;

pub use close::*;
pub use recv::*;
pub use stats::*;
use support::*;
#[cfg(lazyos_tests)]
pub use txn::expire_deadlines;
use txn::*;
pub use types::*;

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

/// Test-only hooks for the kernel suite (issue #62 pattern).
#[cfg(lazyos_tests)]
pub mod harness;

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
    let receivers = enqueue(
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
    wake(&receivers);
    Ok(())
}

/// Enqueue a message into the peer endpoint's inbox, taking the buffer
/// references it carries and metering the sender.
///
/// Returns the tasks parked in `recv` on that inbox (taken off the endpoint);
/// the caller wakes them with [`wake`] once the registry lock is released.
fn enqueue(channel_id: u64, from_side: usize, message: Queued) -> Result<Vec<usize>, Error> {
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
    let receivers = core::mem::take(&mut endpoint.waiters);
    meter(channel, sender).sent += 1;
    Ok(receivers)
}

/// Start a synchronous call (section 6): register a fresh `txn_id` and enqueue
/// the request. The caller stays runnable; [`await_reply`] is the half that
/// blocks until the transaction ends.
///
/// Returns the transaction id; the caller completes it with [`await_reply`].
/// A request that cannot even be queued (wrong handle, malformed parcel, full
/// queue, dead peer, nested cycle) fails before the transaction is created.
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
    let receivers = {
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
        let receivers = core::mem::take(&mut endpoint.waiters);
        channel.calls += 1;
        let sender = meter(channel, me);
        sender.sent += 1;
        sender.calls += 1;
        sender.outstanding += 1;
        receivers
    };
    // Wake the callee: it may already be parked in `recv`, and nothing else
    // wakes it for this request. `send` does the same; the userspace
    // `messengerd` round trip depends on it.
    wake(&receivers);
    // The caller stays runnable: `call` parks in `await_reply` (in the same
    // interrupts-off syscall, so no reply can slip in before the first wait),
    // and the two-step syscall form (`OP_CALL_BEGIN`, `OP_CALL_AWAIT`) must
    // return to user mode runnable. Parking here left a task that begins a
    // call and then serves its own request (messengerd's self-soak) marked
    // blocked in user mode; with targeted wakeups nothing else ever woke it,
    // so a timer tick in that window stranded it forever (issue #338).
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
        // This is the only park of a call: `begin_call` left us runnable, and
        // the queue drops our entry when the wake is consumed.
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
    let mut found = None;
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
            found = Some(caller);
            break;
        }
    }
    // Only the caller waits for this outcome (in `await_reply`, or still
    // parked by `begin_call`).
    let caller = found.ok_or(Error::NoTransaction)?;
    wake(&[caller]);
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
    // The canceller is the caller, which is running; `await_reply` sees the
    // Canceled outcome on its next check without any wake.
    Ok(())
}
