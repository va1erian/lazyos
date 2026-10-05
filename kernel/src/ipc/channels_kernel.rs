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

use super::{enqueue, handles, split_object_id, wake, Error, HandleKind, Queued};

/// The channel side a handle in `slot`'s table names, as `(channel_id, side)`,
/// provided nobody else can read it (issue #283).
///
/// Kernel-stamped interrupt messages land in the inbox of the named side, and
/// whoever holds that side reads them. A name resolve hands every client a
/// handle to the *same* endpoint side, so a driver could otherwise name a side
/// of a service it merely resolved and make that service receive
/// kernel-stamped messages. The side must therefore be the claimant's own:
/// held by this one handle and no other, in any table. Call [`seal_endpoint`]
/// once the binding is committed so it stays that way.
///
/// The pair is a stable identity: channel ids are never reused, so a later post
/// to a closed channel fails with [`Error::InvalidHandle`] or
/// [`Error::PeerDied`] instead of reaching a stranger.
pub fn private_endpoint_of_task(slot: usize, handle: u64) -> Result<(u64, usize), Error> {
    let entry = handles::get_for_task(slot, handle).map_err(super::from_handles)?;
    if entry.kind != HandleKind::Channel {
        return Err(Error::WrongKind);
    }
    if handles::object_refs(HandleKind::Channel, entry.object_id) != 1 {
        return Err(Error::MissingRight);
    }
    Ok(split_object_id(entry.object_id))
}

/// Drop the `DUPLICATE`/`TRANSFER` rights of an endpoint bound for interrupts,
/// so the private side [`private_endpoint_of_task`] vetted cannot be spread.
pub fn seal_endpoint(slot: usize, handle: u64) {
    let spread = handles::rights::DUPLICATE | handles::rights::TRANSFER;
    let _ = handles::drop_rights_for_task(slot, handle, spread);
}

/// Enqueue `parcel_bytes` into the inbox of `side` of `channel_id` as a one-way
/// message from the kernel task, then wake the tasks parked on it.
///
/// The parcel is validated exactly like a userspace send would be.
pub fn post_from_kernel(channel_id: u64, side: usize, parcel_bytes: &[u8]) -> Result<(), Error> {
    let parcel = super::validate_parcel(parcel_bytes)?;
    let queued = Queued {
        sender: crate::task::KERNEL_TASK,
        origin: super::SenderId::KERNEL,
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
    let receivers = enqueue(channel_id, (side & 1) ^ 1, queued)?;
    wake(receivers.iter());
    Ok(())
}
