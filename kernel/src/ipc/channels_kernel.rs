//! Kernel-originated one-way messages (issue #240).
//!
//! The device core posts an interrupt notification into a driver's endpoint
//! *as the kernel*: the message carries `sender == KERNEL_TASK` (slot 0), which
//! the receiver can trust because senders cannot forge it. There is no handle
//! on the sending side, so this bypasses the per-task handle table and the ACL
//! hook by design (the kernel is not a Messenger client); the destination was
//! validated once, when the driver named it at `claim`.
//!
//! This is a child of `channels` so it can reach the private registry; it lives
//! in its own file only to keep `channels.rs` from growing.

use alloc::vec::Vec;

use super::{enqueue, handles, split_object_id, Error, HandleKind, Queued, MESSENGER};

/// The channel side a handle in `slot`'s table names, as `(channel_id, side)`.
///
/// Requires a `Channel` handle. The pair is a stable identity: channel ids are
/// never reused, so a later post to a closed channel fails with
/// [`Error::InvalidHandle`] or [`Error::PeerDied`] instead of reaching a
/// stranger.
pub fn endpoint_of_task(slot: usize, handle: u64) -> Result<(u64, usize), Error> {
    let entry = handles::get_for_task(slot, handle).map_err(super::from_handles)?;
    if entry.kind != HandleKind::Channel {
        return Err(Error::WrongKind);
    }
    Ok(split_object_id(entry.object_id))
}

/// Enqueue `parcel_bytes` into the inbox of `side` of `channel_id` as a one-way
/// message from the kernel task, then wake receivers.
///
/// The parcel is validated exactly like a userspace send would be.
pub fn post_from_kernel(channel_id: u64, side: usize, parcel_bytes: &[u8]) -> Result<(), Error> {
    let parcel = super::validate_parcel(parcel_bytes)?;
    let queued = Queued {
        sender: crate::task::KERNEL_TASK,
        quota_uid: 0,
        method: parcel.header.method,
        flags: parcel.header.flags,
        txn: None,
        deadline: None,
        bytes: parcel_bytes.to_vec(),
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    // `enqueue` names the *sending* side and delivers to its peer, so name the
    // opposite side of the inbox we want to fill.
    enqueue(channel_id, (side & 1) ^ 1, queued)?;
    MESSENGER.notify_all();
    Ok(())
}
