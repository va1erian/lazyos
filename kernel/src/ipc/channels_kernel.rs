//! Kernel-originated one-way messages and the channels they travel on (issues
//! #240, #496).
//!
//! The device core posts an interrupt notification into a driver's inbox *as
//! the kernel*: the message carries `sender == KERNEL_TASK` (slot 0), which
//! the receiver can trust because senders cannot forge it. There is no handle
//! on the sending side, so this bypasses the per-task handle table and the ACL
//! hook by design (the kernel is not a Messenger client).
//!
//! # The interrupt channel kind (issue #496)
//!
//! Kernel-stamped messages land in the inbox of one channel side, and whoever
//! holds that side reads them. When a driver named that side itself, the
//! kernel could only check the handle tables, so a side published in the name
//! registry or duplicated into a message still in flight could be bound and
//! then read (or held) by someone else. The side is therefore never the
//! driver's to name: [`create_irq_channel`] makes a fresh channel whose
//! sending side is held by the kernel alone (no handle anywhere, nothing can
//! be sent into it) and gives the claimant the receiving side with `CALL`
//! only, so it can receive and wait on it but never duplicate, transfer or
//! publish it. The device core closes the kernel's side when the claim goes
//! ([`close_kernel_side`]); the driver then sees `PeerDied` once its inbox
//! drains.

use alloc::vec::Vec;

use super::{
    enqueue, fresh_channel, from_handles, handles, object_id, split_object_id, wake, Error,
    HandleKind, Queued, CHANNELS,
};

/// The side of an interrupt channel the kernel keeps; the claimant gets the
/// other one.
const KERNEL_SIDE: usize = 0;
/// The side the claimant receives on.
pub const IRQ_SIDE: usize = 1;

/// Create an interrupt channel for the current task: returns the channel id
/// and the task's handle to its receiving side ([`IRQ_SIDE`]), which carries
/// `CALL` (receive and wait) and nothing else.
pub fn create_irq_channel() -> Result<(u64, u64), Error> {
    let channel_id = CHANNELS.lock().insert(|id| {
        let mut channel = fresh_channel(id);
        channel.endpoints[KERNEL_SIDE].kernel_held = true;
        channel
    })?;
    match handles::open(
        HandleKind::Channel,
        handles::rights::CALL,
        object_id(channel_id, IRQ_SIDE),
    ) {
        Ok(handle) => Ok((channel_id, handle)),
        Err(error) => {
            CHANNELS.lock().remove(channel_id);
            Err(from_handles(error))
        }
    }
}

/// Close the kernel's side of interrupt channel `channel_id` (its claim is
/// gone): the receiver sees `PeerDied` once it has drained what was queued,
/// and the channel goes once the receiver's handle closes too. A channel that
/// is not an interrupt channel is left alone.
pub fn close_kernel_side(channel_id: u64) {
    let ours = CHANNELS
        .lock()
        .get(channel_id)
        .is_some_and(|channel| channel.endpoints[KERNEL_SIDE].kernel_held);
    if ours {
        let orphans = super::close::close_side(channel_id, KERNEL_SIDE);
        super::close::close_orphans(orphans);
    }
}

/// Whether the endpoint `object` (a handle's object id) belongs to an
/// interrupt channel: such a side may never be published.
pub fn is_irq_channel(object: u64) -> bool {
    let (channel_id, _) = split_object_id(object);
    CHANNELS
        .lock()
        .get(channel_id)
        .is_some_and(|channel| channel.endpoints[KERNEL_SIDE].kernel_held)
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
        objects: Vec::new(),
    };
    // `enqueue` names the *sending* side and delivers to its peer, so name the
    // opposite side of the inbox we want to fill.
    let receivers = enqueue(channel_id, (side & 1) ^ 1, queued)?;
    wake(receivers.iter());
    super::ring(channel_id, side & 1);
    Ok(())
}

/// Test builds: post `parcel_bytes` as the kernel into the inbox behind the
/// current task's channel handle `handle` (to fill an interrupt inbox).
#[cfg(lazyos_tests)]
pub fn post_into_handle(handle: u64, parcel_bytes: &[u8]) -> Result<(), Error> {
    let entry = handles::get(handle).map_err(from_handles)?;
    if entry.kind != HandleKind::Channel {
        return Err(Error::WrongKind);
    }
    let (channel_id, side) = split_object_id(entry.object_id);
    // The channel must exist (a stale handle names nothing).
    super::find_channel(&mut CHANNELS.lock(), channel_id)?;
    post_from_kernel(channel_id, side, parcel_bytes)
}
