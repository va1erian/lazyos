//! Channel helpers: id packing, parcel validation, object resolution and quota release.

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
        timeouts: 0,
        polls: 0,
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

/// End the pending transaction at `index` as `TimedOut` and count why: an
/// unanswered poll (`poll`) ticks `polls`, a missed real deadline ticks
/// `timeouts` (channel and caller's meter alike). Returns the caller to wake.
pub(super) fn time_out(channel: &mut Channel, index: usize, poll: bool) -> usize {
    let txn = &mut channel.txns[index];
    txn.state = TxnState::TimedOut;
    let caller = txn.caller;
    if poll {
        channel.polls += 1;
    } else {
        channel.timeouts += 1;
    }
    let sender = meter(channel, caller);
    if poll {
        sender.polls += 1;
    } else {
        sender.timeouts += 1;
    }
    release_pending(channel, caller);
    caller
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

/// Resolve a parcel's object list against the sending task's table
/// (`docs/messenger-core-plan.md` 2.3): the list must equal what the
/// header's method declares (`declared`), each entry's handle must be of the
/// entry's kind and carry `TRANSFER`, and a channel end may appear once. No
/// reference is taken here: [`retain_objects`] runs once the message is
/// accepted for queueing, so a refused send changes nothing.
pub(super) fn resolve_objects(parcel: &ParcelView<'_>) -> Result<Vec<Resolved>, Error> {
    declared::check_declared(parcel)?;
    let mut resolved = Vec::with_capacity(parcel.object_count());
    for (index, object) in parcel.objects().enumerate() {
        let entry = handles::get(object.handle()).map_err(from_handles)?;
        let kind = match (object, entry.kind) {
            (Object::Channel(_), HandleKind::Channel) => ObjectKind::Channel,
            (Object::Buffer(_), HandleKind::Buffer) => ObjectKind::Buffer,
            // A buffer in a channel slot or a channel in a buffer slot: the
            // kernel would move what should be shared, or the reverse.
            _ => return Err(Error::WrongObjectKind),
        };
        if entry.rights & rights::TRANSFER == 0 {
            return Err(Error::MissingRight);
        }
        if kind == ObjectKind::Channel
            && parcel
                .objects()
                .take(index)
                .any(|earlier| earlier.handle() == object.handle())
        {
            // One handle, one move: a duplicate entry would install two
            // receiver handles from a single reference.
            return Err(Error::BadTransfer);
        }
        resolved.push(Resolved {
            kind,
            rights: entry.rights,
            object_id: entry.object_id,
        });
    }
    Ok(resolved)
}

/// The sender's handle numbers of the channel ends `parcel` moves: closed
/// once the message is queued ([`close_moved_handles`]).
pub(super) fn moved_handles(parcel: &ParcelView<'_>) -> Vec<u64> {
    parcel
        .objects()
        .filter_map(|object| match object {
            Object::Channel(handle) => Some(handle),
            Object::Buffer(_) => None,
        })
        .collect()
}

/// Take one registry reference per buffer the message carries, releasing what
/// was already taken if a later entry fails.
pub(super) fn retain_objects(message: &Queued) -> Result<(), Error> {
    let mut retained: Vec<u64> = Vec::new();
    for buffer in buffers(&message.objects) {
        if let Err(error) = shared::retain(buffer.object_id) {
            for object_id in &retained {
                shared::release(*object_id);
            }
            return Err(from_shared(error));
        }
        retained.push(buffer.object_id);
    }
    Ok(())
}

/// The shared buffers among `objects`.
pub(super) fn buffers(objects: &[Resolved]) -> impl Iterator<Item = &Resolved> + '_ {
    objects
        .iter()
        .filter(|object| object.kind == ObjectKind::Buffer)
}

/// The channel endpoints among `objects` (their object ids).
pub(super) fn channel_objects(objects: &[Resolved]) -> impl Iterator<Item = u64> + '_ {
    objects
        .iter()
        .filter(|object| object.kind == ObjectKind::Channel)
        .map(|object| object.object_id)
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
    for buffer in buffers(&message.objects) {
        shared::release(buffer.object_id);
    }
}

/// Finish a channel move: the sender's numbers were resolved into the
/// message, so close the sender's handles now that the message is safely
/// queued.
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
