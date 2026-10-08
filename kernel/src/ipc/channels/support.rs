//! Channel helpers: id packing, parcel validation, transfer retention and quota release.

use super::*;

/// Pack the endpoint name into a handle's `object_id`.
pub(super) fn object_id(channel_id: u64, side: usize) -> u64 {
    (channel_id << 1) | side as u64
}

/// Unpack a handle's `object_id` into `(channel_id, side)`.
pub(super) fn split_object_id(object: u64) -> (u64, usize) {
    (object >> 1, (object & 1) as usize)
}

/// Find a channel in the registry, or report a stale handle.
pub(super) fn find_channel(channels: &mut Registry, id: u64) -> Result<&mut Channel, Error> {
    channels.get_mut(id).ok_or(Error::InvalidHandle)
}

/// Find a channel in the registry without mutating it.
pub(super) fn find_channel_ref(channels: &Registry, id: u64) -> Result<&Channel, Error> {
    channels.get(id).ok_or(Error::InvalidHandle)
}

/// Borrow the meter for `slot`, creating it on first use.
pub(super) fn meter(channel: &mut Channel, slot: usize) -> &mut SenderMeter {
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
pub(super) fn release_pending(channel: &mut Channel, caller: usize) {
    if let Some(meter) = channel
        .senders
        .iter_mut()
        .find(|meter| meter.slot == caller)
    {
        meter.outstanding = meter.outstanding.saturating_sub(1);
    }
}

/// Validate a parcel at the kernel boundary, in place (P6.3: the bytes are
/// queued as they arrived, nothing is decoded into copies). Parsing is the
/// attack surface; this never panics and rejects anything over the wire
/// limits.
pub(super) fn validate_parcel(bytes: &[u8]) -> Result<ParcelView<'_>, Error> {
    if bytes.len() > libmessenger::MAX_PARCEL_BYTES {
        return Err(Error::BadParcel);
    }
    ParcelView::parse(bytes).map_err(|_| Error::BadParcel)
}

/// Resolve a parcel's `handles` and `buffers` against the sending task's
/// table, validating the declaration (issue #516), kinds, rights and the
/// per-message limits. No reference is taken here: [`retain_transfers`] runs
/// once the message is accepted for queueing, so a refused send changes
/// nothing.
///
/// A buffer travels only in `buffers` (`docs/messenger-core-plan.md` 3.4):
/// a `Buffer` handle in `handles` is refused before anything moves, so the
/// handle vector only ever carries endpoints, channels and objects.
pub(super) fn resolve_transfers(
    parcel: &ParcelView<'_>,
) -> Result<(Vec<Transfer>, Vec<BufferTransfer>), Error> {
    declared::check_declared(parcel)?;
    if parcel.handle_count() > libmessenger::MAX_HANDLES
        || parcel.buffer_count() > libmessenger::MAX_BUFFERS
    {
        return Err(Error::BadParcel);
    }
    let mut transfers = Vec::with_capacity(parcel.handle_count());
    for (index, local) in parcel.handles().enumerate() {
        if parcel.handles().take(index).any(|earlier| earlier == local) {
            // One handle, one move: a duplicate entry would install two
            // receiver handles from a single reference.
            return Err(Error::BadTransfer);
        }
        let entry = handles::get(local).map_err(from_handles)?;
        if entry.kind == HandleKind::Buffer {
            return Err(Error::BufferInHandles);
        }
        if entry.rights & rights::TRANSFER == 0 {
            return Err(Error::MissingRight);
        }
        transfers.push(Transfer {
            kind: entry.kind,
            rights: entry.rights,
            object_id: entry.object_id,
        });
    }
    let mut buffers = Vec::with_capacity(parcel.buffer_count());
    for descriptor in parcel.buffers() {
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
pub(super) fn retain_transfers(message: &Queued) -> Result<(), Error> {
    let mut retained: Vec<u64> = Vec::new();
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
pub(super) fn charge_queued(uid: u32, bytes: usize) -> Result<(), Error> {
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
/// the uid in [`Queued::origin`] so a delivery after a credential transition still
/// credits the user that was charged.
pub(super) fn release_queued_quota(uid: u32, bytes: usize) {
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
pub(super) fn release_queued(message: &Queued) {
    for buffer in &message.buffers {
        shared::release(buffer.object_id);
    }
}

/// Finish a handle move: the sender's numbers were resolved into the message,
/// so close the sender's handles now that the message is safely queued.
pub(super) fn close_moved_handles(numbers: &[u64]) {
    for local in numbers {
        handles::close(*local).ok();
    }
}

/// Resolve a handle to `(channel_id, side)`, checking kind and rights.
pub(super) fn endpoint_of(handle: u64, required: u32) -> Result<(u64, usize), Error> {
    let entry = handles::get(handle).map_err(from_handles)?;
    if entry.kind != HandleKind::Channel {
        return Err(Error::WrongKind);
    }
    if entry.rights & required != required {
        return Err(Error::MissingRight);
    }
    Ok(split_object_id(entry.object_id))
}
