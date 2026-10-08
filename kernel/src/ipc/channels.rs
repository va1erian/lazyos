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
//!
//! A request carries at most what its `.midl` method declares (`transfers
//! (...)`, issue #516): [`declared`] checks the parcel header's interface and
//! method against the generated table before anything moves, and refuses the
//! rest with [`Error::UndeclaredTransfer`]. An unknown interface declares none.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;

use libmessenger::{flags, BufferDesc, ParcelView};

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

mod call;
mod close;
mod connect;
pub mod declared;
mod pollstate;
mod recv;
mod registry;
mod stats;
mod support;
mod txn;
mod types;

pub use call::*;
pub use close::*;
pub use connect::*;
pub use pollstate::{ring, side_state};
pub use recv::*;
use registry::*;
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

/// The call deadline that means "poll": answer if the callee can do so in its
/// current service turn, otherwise report `TimedOut` without blocking for
/// long. Equal to the user library's `EXPIRED_DEADLINE`.
///
/// A literally expired deadline cannot express that: the caller would time
/// out inside `await_reply` before the callee ever ran, so the callee's reply
/// was always refused as "caller gone" and a queued event was never delivered
/// (messengerd's topic polls). A poll instead stays open while the callee
/// serves it and ends when the callee comes back to `recv` (see
/// `recv::expire_served_polls`), or after [`POLL_GRACE_TICKS`] if the callee
/// never picks it up.
pub const POLL_DEADLINE: u64 = 1;
/// Longest a poll waits for a callee that has not yet received it (PIT ticks,
/// 100 Hz): a busy or stalled service must not stall its pollers.
pub const POLL_GRACE_TICKS: u64 = 3;
/// Longest a poll stays open once the callee has received it (PIT ticks): the
/// callee gets its whole service turn, however slow, but a callee that wedges
/// mid-request still cannot hold its poller forever. Receipt replaces the
/// grace deadline with this one (`recv::take_locked`).
pub const POLL_SERVICE_TICKS: u64 = 100;

/// Global channel registry, exactly the "pairs of bounded queues" shape of
/// section 14, indexed by the slot every channel and transaction id carries
/// (`registry`, P6.3). Replies are matched by transaction id alone.
static CHANNELS: Mutex<Registry> = Mutex::new(Registry::new());

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
    let channel_id = CHANNELS.lock().insert(fresh_channel)?;
    let first = match handles::open(HandleKind::Channel, rights::ALL, object_id(channel_id, 0)) {
        Ok(handle) => handle,
        Err(error) => {
            CHANNELS.lock().remove(channel_id);
            return Err(from_handles(error));
        }
    };
    match handles::open(HandleKind::Channel, rights::ALL, object_id(channel_id, 1)) {
        Ok(second) => Ok((first, second)),
        Err(error) => {
            handles::close(first).ok();
            CHANNELS.lock().remove(channel_id);
            Err(from_handles(error))
        }
    }
}

/// Send a one-way message (section 7.1): enqueue and return immediately.
///
/// Ordering is preserved per sender to this channel and delivery is at most
/// once; a full queue is refused rather than blocking.
pub fn send(handle: u64, parcel_bytes: &[u8]) -> Result<(), Error> {
    send_owned(handle, parcel_bytes.to_vec())
}

/// [`send`] that queues `parcel_bytes` itself instead of a copy (P6.3).
pub fn send_owned(handle: u64, parcel_bytes: Vec<u8>) -> Result<(), Error> {
    let me = task::current();
    let (channel_id, side) = endpoint_of(handle, rights::CALL)?;
    let parcel = validate_parcel(&parcel_bytes)?;
    let (method, parcel_flags) = (parcel.header.method, parcel.header.flags);
    let (handles, buffers) = resolve_transfers(&parcel)?;
    let numbers: Vec<u64> = parcel.handles().collect();
    let kinds: Vec<HandleKind> = handles.iter().map(|transfer| transfer.kind).collect();
    let receivers = enqueue(
        channel_id,
        side,
        Queued {
            sender: me,
            origin: SenderId::of(me),
            method,
            flags: parcel_flags,
            txn: None,
            deadline: None,
            bytes: parcel_bytes,
            handles,
            buffers,
        },
    )?;
    // The message owns the moved references now; the sender's numbers are gone.
    close_moved_handles(&numbers, &kinds);
    wake(receivers.iter());
    ring(channel_id, 1 - side);
    Ok(())
}

/// Enqueue a message into the peer endpoint's inbox, taking the buffer
/// references it carries and metering the sender.
///
/// Returns the tasks parked in `recv` on that inbox (taken off the endpoint);
/// the caller wakes them with [`wake`] once the registry lock is released.
fn enqueue(channel_id: u64, from_side: usize, message: Queued) -> Result<WaiterSet, Error> {
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
    if let Err(error) = charge_queued(message.origin.uid, message.bytes.len()) {
        channel.drops += 1;
        return Err(error);
    }
    // The queue has room: take the message's buffer references so a sender
    // that closes its own handle cannot free frames an in-flight message needs.
    if let Err(error) = retain_transfers(&message) {
        release_queued_quota(message.origin.uid, message.bytes.len());
        channel.drops += 1;
        return Err(error);
    }
    let sender = message.sender;
    let bytes = message.bytes.len();
    let endpoint = &mut channel.endpoints[peer];
    endpoint.inbox.push_back(message);
    endpoint.queued_bytes += bytes;
    endpoint.arrivals += 1;
    let receivers = endpoint.waiters.take();
    meter(channel, sender).sent += 1;
    Ok(receivers)
}
