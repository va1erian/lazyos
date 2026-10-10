//! Receiving and delivering queued messages.

use super::*;

mod waitcall;
mod waitset;
pub use waitcall::*;
pub use waitset::*;

/// Receive the next message without blocking; `Ok(None)` means "try later".
///
/// Delivery installs the message's objects into the calling task's handle
/// table and reports them as local numbers, so the returned [`Message`] is
/// immediately usable.
pub fn try_recv(handle: u64) -> Result<Option<Message>, Error> {
    let (channel_id, side) = endpoint_of(handle, rights::CALL)?;
    take_or_register(channel_id, side, None)
}

/// [`try_recv`] on a resolved endpoint. When the inbox is empty and `waiter`
/// is set, register it as parked on this side *under the same lock* that saw
/// the inbox empty, so no delivery can slip between the check and the
/// registration.
fn take_or_register(
    channel_id: u64,
    side: usize,
    waiter: Option<usize>,
) -> Result<Option<Message>, Error> {
    let mut abandoned = Vec::new();
    let taken = take_locked(channel_id, side, waiter, &mut abandoned);
    // Woken with `CHANNELS` released (queue-then-task lock order).
    wake(abandoned);
    let queued = taken?;
    match queued {
        Some(message) => {
            // Room in this inbox is what the other side's `POLLOUT` reports.
            ring(channel_id, 1 - side);
            // Delivery takes the message out of the inbox, so the sender's
            // user gets the queue charge back (issue #103).
            release_queued_quota(message.origin.uid, message.bytes.len());
            let poll = match (message.txn, message.deadline) {
                (Some(txn), Some(POLL_DEADLINE)) => Some(txn),
                _ => None,
            };
            match deliver(message) {
                Ok(message) => Ok(Some(message)),
                Err(error) => {
                    // The callee never saw the poll: it is not being served.
                    if let Some(txn) = poll {
                        unserve_poll(channel_id, side, txn);
                    }
                    Err(error)
                }
            }
        }
        None => Ok(None),
    }
}

/// Undo [`take_locked`]'s receipt of poll `txn` after its delivery failed:
/// back on the grace deadline, no longer counted as served, so it ends as an
/// empty poll rather than as a callee that wedged.
fn unserve_poll(channel_id: u64, side: usize, txn: u64) {
    let mut channels = CHANNELS.lock();
    let Ok(channel) = find_channel(&mut channels, channel_id) else {
        return;
    };
    channel.endpoints[side]
        .serving_polls
        .retain(|id| *id != txn);
    if let Some(entry) = channel.txns.iter_mut().find(|entry| entry.id == txn) {
        if entry.state == TxnState::Pending {
            entry.served = false;
            entry.deadline = Some(task::ticks() + POLL_GRACE_TICKS);
        }
    }
}

/// The locked half of [`take_or_register`]. Callers of polls the receiver had
/// left unanswered are pushed onto `abandoned` for the caller to wake.
fn take_locked(
    channel_id: u64,
    side: usize,
    waiter: Option<usize>,
    abandoned: &mut Vec<usize>,
) -> Result<Option<Queued>, Error> {
    let mut channels = CHANNELS.lock();
    let channel = find_channel(&mut channels, channel_id)?;
    // Coming back to receive means the receiver is done with its previous
    // message: any poll it neither answered nor is still working on is over.
    expire_served_polls(channel, side, abandoned);
    let endpoint = &mut channel.endpoints[side];
    if let Some(message) = endpoint.inbox.pop_front() {
        endpoint.queued_bytes = endpoint.queued_bytes.saturating_sub(message.bytes.len());
        if let (Some(txn), Some(POLL_DEADLINE)) = (message.txn, message.deadline) {
            endpoint.serving_polls.push(txn);
            // Received: the grace deadline no longer applies, or a slow
            // service turn would have its reply refused. The parked caller
            // wakes at the old deadline, finds it replaced and parks again
            // (`await_reply` only expires a deadline that is actually due).
            if let Some(entry) = channel.txns.iter_mut().find(|entry| entry.id == txn) {
                if entry.state == TxnState::Pending {
                    entry.served = true;
                    entry.deadline = Some(task::ticks() + POLL_SERVICE_TICKS);
                }
            }
        }
        Ok(Some(message))
    } else if channel.endpoints[1 - side].closed {
        Err(Error::PeerDied)
    } else {
        if let Some(slot) = waiter {
            add_waiter(&mut channel.endpoints[side], slot);
        }
        Ok(None)
    }
}

/// End every poll `side` received earlier and never answered: the receiver
/// has returned to `recv`, so it answered (the transaction is already
/// `Replied`) or deferred the request (a parked pull), and a poll must not
/// wait for a deferred answer. Callers are appended to `woken`.
fn expire_served_polls(channel: &mut Channel, side: usize, woken: &mut Vec<usize>) {
    let served = core::mem::take(&mut channel.endpoints[side].serving_polls);
    for id in served {
        let Some(index) = channel.txns.iter().position(|txn| txn.id == id) else {
            continue;
        };
        if channel.txns[index].state != TxnState::Pending {
            continue;
        }
        // The callee came back to `recv` without answering: "nothing there",
        // unless the service bound had already passed (a callee that was
        // slow, like the deadline sweep would have counted it).
        let overdue = channel.txns[index]
            .deadline
            .is_some_and(|deadline| deadline <= task::ticks());
        woken.push(time_out(channel, index, !overdue));
    }
}

/// Install a queued message's objects into the receiving task's handle table,
/// in object-list order, as local numbers.
///
/// On failure (the receiver is out of handles) everything installed is rolled
/// back and the references of everything still pending are released, so a
/// failed delivery cannot leak handles or frames.
pub(super) fn deliver(queued: Queued) -> Result<Message, Error> {
    let mut installed: Vec<u64> = Vec::with_capacity(queued.objects.len());
    for object in queued.objects.iter() {
        let opened = match object.kind {
            ObjectKind::Channel => {
                handles::open(HandleKind::Channel, object.rights, object.object_id)
                    .map_err(from_handles)
            }
            // The message's reference becomes the receiver's handle.
            ObjectKind::Buffer => {
                shared::attach(object.object_id, object.rights).map_err(from_shared)
            }
        };
        match opened {
            Ok(handle) => installed.push(handle),
            Err(error) => {
                rollback_delivery(&queued, &installed);
                return Err(error);
            }
        }
    }
    Ok(Message {
        sender: queued.sender,
        origin: queued.origin,
        method: queued.method,
        flags: queued.flags,
        txn: queued.txn,
        deadline: queued.deadline,
        bytes: queued.bytes,
        objects: installed,
    })
}

/// Undo a partial [`deliver`]: close the handles installed so far (a buffer
/// handle's close drops the reference `attach` converted) and release the
/// message references of every buffer still pending.
pub(super) fn rollback_delivery(queued: &Queued, installed: &[u64]) {
    for (object, &handle) in queued.objects.iter().zip(installed) {
        match object.kind {
            ObjectKind::Channel => handles::close(handle).ok(),
            ObjectKind::Buffer => shared::close(handle).ok(),
        };
    }
    for buffer in buffers(&queued.objects[installed.len()..]) {
        shared::release(buffer.object_id);
    }
    // A moved channel end nobody received is closed, so its peer learns.
    close_orphans(channel_objects(&queued.objects).collect());
}

/// Receive the next message, parking until one arrives, the deadline passes, or
/// the peer closes.
///
/// Only a delivery to (or a close of) this endpoint wakes the caller
/// (issue #338); the wake is still advisory and the inbox is re-checked.
pub fn recv(handle: u64, deadline: Option<u64>) -> Result<Message, Error> {
    let (channel_id, side) = endpoint_of(handle, rights::CALL)?;
    let me = task::current();
    loop {
        match take_or_register(channel_id, side, Some(me)) {
            Ok(Some(message)) => return Ok(message),
            Ok(None) => {}
            Err(error) => return Err(error),
        }
        let reason = MESSENGER.wait(me, deadline);
        // A waker takes the whole list; a timeout (or any other return)
        // must drop our own registration so it cannot outlive this call.
        remove_waiter(channel_id, side, me);
        if reason == WakeReason::TimedOut {
            return Err(Error::TimedOut);
        }
        // A fatal signal (a supervisor's `SIGTERM` during an orderly
        // shutdown) must reach the syscall return, where the native gate ends
        // the task; parking again would keep it alive until `SIGKILL`. The
        // task never sees this error.
        if reason == WakeReason::Interrupted
            && (task::signal::killed(me) || task::signal::native_fatal_pending(me).is_some())
        {
            return Err(Error::Canceled);
        }
    }
}

/// Record `slot` as parked on `endpoint` (at most once).
pub(super) fn add_waiter(endpoint: &mut Endpoint, slot: usize) {
    endpoint.waiters.insert(slot);
}

/// Drop `slot`'s registration on one side, if the channel still exists.
fn remove_waiter(channel_id: u64, side: usize, slot: usize) {
    let mut channels = CHANNELS.lock();
    if let Ok(channel) = find_channel(&mut channels, channel_id) {
        channel.endpoints[side].waiters.remove(slot);
    }
}

/// Wake each task in `slots` that is still parked on the Messenger queue,
/// returning the first one that actually woke.
///
/// Called with `CHANNELS` released (queue-then-task lock order). A slot that
/// already returned, timed out or parked elsewhere is skipped by
/// `notify_task`, so a stale registration can never wake an unrelated wait.
pub(super) fn wake(slots: impl IntoIterator<Item = usize>) -> Option<usize> {
    let mut first = None;
    for slot in slots {
        if MESSENGER.notify_task(slot) && first.is_none() {
            first = Some(slot);
        }
    }
    first
}

/// Run `partner` (a callee a call woke, a caller a reply woke) as soon as the
/// current task parks: the direct handoff of P6.2 (`task::hand_off`).
pub(super) fn hand_off_to(partner: Option<usize>) {
    if let Some(partner) = partner {
        task::hand_off(partner);
    }
}
