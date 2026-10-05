//! Closing endpoints and forgetting a task's channels.

use super::*;

/// Forget everything a reclaimed task slot still owns inside the channel
/// registry: transactions it started (nobody will ever collect their outcome)
/// and its per-channel sender meters. Called by task teardown after the slot's
/// endpoints were closed.
pub fn forget_task(slot: usize) {
    let mut channels = CHANNELS.lock();
    for channel in channels.iter_mut() {
        channel.txns.retain(|txn| txn.caller != slot);
        channel.senders.retain(|meter| meter.slot != slot);
        // A task that died parked in `recv` never unregistered itself; the
        // slot may be reused, so drop the stale entry.
        for endpoint in channel.endpoints.iter_mut() {
            endpoint.waiters.remove(slot);
        }
    }
    drop(channels);
    // A task that died parked in `recv` or `await_reply` is still on the
    // Messenger queue: its wait loop will never remove it.
    MESSENGER.forget(slot);
    // The same for every doorbell and wait queue a dead task may have been
    // parked on: its wait loop never ran to withdraw the registration, and the
    // slot may be reused. Long-lived per-slot state (the `WAIT_FD` watcher
    // count, poll interests) would otherwise leak, and a stale entry could
    // spuriously wake the slot's next owner.
    forget_fd_watcher(slot);
    crate::task::pollwait::forget_task(slot);
    crate::input::bus::disarm_doorbell(slot);
    crate::display::disarm_key_doorbell(slot);
    crate::ipc::inet::bell::disarm(slot);
    crate::task::childbell::disarm(slot);
    crate::task::wait::POLL.forget(slot);
    crate::task::wait::SLEEP.forget(slot);
    crate::task::wait::TERMINAL.forget(slot);
    crate::task::wait::CHILD_EXIT.forget(slot);
    crate::task::wait::SLOT.forget(slot);
}

/// Close one endpoint: drop its handle, mark the side closed, and fail every
/// transaction that still needs it with `PeerDied` (section 9's peer death).
///
/// Messages already queued for the surviving side stay deliverable; once that
/// inbox drains, `recv`/`try_recv` report `PeerDied` too.
pub fn close_endpoint(handle: u64) -> Result<(), Error> {
    close_endpoint_for(task::current(), handle, false)
}

/// Release the caller's handle to an endpoint, closing the side only if no
/// other handle in any table still names it (docs/networking-plan.md N2).
///
/// An explicit [`close_endpoint`] ends the *side* whoever else holds a handle
/// to it, which is right for the owner of a fresh pair and wrong for a handle
/// received from elsewhere: name resolution hands every client a handle to the
/// same side, and a service can be given one of them (a notify endpoint, a
/// reply address). Closing that would end somebody else's service, so a
/// receiver that is done with such a handle releases it instead.
pub fn release_endpoint(handle: u64) -> Result<(), Error> {
    close_endpoint_for(task::current(), handle, true)
}

/// [`close_endpoint`] for a handle in `slot`'s table rather than the caller's.
///
/// Task teardown closes every endpoint a dead task still holds through this,
/// so its peers observe `PeerDied` exactly as they would for a clean close.
/// With `last_holder_only` the side is only marked closed when no other handle
/// in any table still names it: name resolution hands every client a handle to
/// the same side, so a client that exits must not fail its siblings' calls.
pub fn close_endpoint_for(slot: usize, handle: u64, last_holder_only: bool) -> Result<(), Error> {
    let entry = handles::get_for_task(slot, handle).map_err(from_handles)?;
    if entry.kind != HandleKind::Channel {
        return Err(Error::WrongKind);
    }
    let (channel_id, side) = split_object_id(entry.object_id);
    handles::close_for_task(slot, handle).map_err(from_handles)?;
    if last_holder_only && handles::object_refs(HandleKind::Channel, entry.object_id) > 0 {
        return Ok(());
    }
    let mut remove = false;
    let mut woken: Vec<usize> = Vec::new();
    {
        let mut channels = CHANNELS.lock();
        if let Some(channel) = channels.get_mut(channel_id) {
            channel.endpoints[side].closed = true;
            // The surviving side's receivers must observe `PeerDied`, and any
            // other holder parked on the closed side must stop waiting.
            for endpoint in channel.endpoints.iter_mut() {
                woken.extend(endpoint.waiters.take().iter());
            }
            // Anything still queued for the dead side will never be received;
            // release the buffer references those messages hold.
            channel.drops += channel.endpoints[side].inbox.len() as u64;
            let dropped: Vec<Queued> = channel.endpoints[side].inbox.drain(..).collect();
            channel.endpoints[side].queued_bytes = 0;
            for message in &dropped {
                release_queued(message);
                release_queued_quota(message.origin.uid, message.bytes.len());
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
            for &caller in &released {
                release_pending(channel, caller);
            }
            woken.extend(released);
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
                    release_queued_quota(message.origin.uid, message.bytes.len());
                }
            }
        }
        if remove {
            channels.remove(channel_id);
        }
    }
    wake(woken);
    Ok(())
}
