//! What a Linux descriptor watching one endpoint sees (issue #667,
//! `ipc::endpointfd`, docs/architecture/endpoint-fd.md).
//!
//! The descriptor holds no reference into the registry: it asks
//! [`side_state`] on every `poll`/`epoll` scan, and the channel code rings
//! [`ring`] after every change that can make that answer different (a
//! delivery, a receive that freed room in the peer's way, a close). A ring
//! with no endpoint descriptor alive anywhere costs one atomic load.

use super::*;

/// One side's readiness as an endpoint descriptor reports it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SideState {
    /// A message waits in this side's inbox (`recv` would not block).
    pub queued: bool,
    /// The other side is closed: `recv` returns `PeerDied` once the inbox
    /// drains, and nothing more can arrive.
    pub peer_closed: bool,
    /// This side itself was closed (every handle to it gone, or closed
    /// explicitly by a holder).
    pub closed: bool,
    /// A `send` from this side would find room in the peer's inbox.
    pub room: bool,
    /// Messages ever delivered to this side: the edge counter that lets an
    /// edge-triggered `epoll` interest see a new message on a side that
    /// stayed readable.
    pub arrivals: u64,
}

/// The state of `side` of `channel_id`, or `None` once the channel is gone
/// (both sides closed).
pub fn side_state(channel_id: u64, side: usize) -> Option<SideState> {
    let channels = CHANNELS.lock();
    let channel = find_channel_ref(&channels, channel_id).ok()?;
    let mine = &channel.endpoints[side & 1];
    let peer = &channel.endpoints[(side & 1) ^ 1];
    Some(SideState {
        queued: !mine.inbox.is_empty(),
        peer_closed: peer.closed,
        closed: mine.closed,
        room: peer.inbox.len() < MAX_QUEUE_DEPTH && peer.queued_bytes < MAX_QUEUE_BYTES,
        arrivals: mine.arrivals,
    })
}

/// Tell the endpoint descriptors watching `side` of `channel_id` that its
/// state may have changed. Call with `CHANNELS` released: the wake takes the
/// poll queue and the task table (queue-then-task lock order).
pub fn ring(channel_id: u64, side: usize) {
    crate::ipc::endpointfd::ring(object_id(channel_id, side & 1));
}

/// [`ring`] both sides: a close changes what each of them reports.
pub(super) fn ring_both(channel_id: u64) {
    ring(channel_id, 0);
    ring(channel_id, 1);
}
