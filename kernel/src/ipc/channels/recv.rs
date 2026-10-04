//! Receiving and delivering queued messages.

use super::*;

mod waitset;
pub use waitset::*;

/// Receive the next message without blocking; `Ok(None)` means "try later".
///
/// Delivery installs the message's transferred handles and buffers into the
/// calling task's handle table and rewrites them to local numbers, so the
/// returned [`Message`] is immediately usable.
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
    wake(&abandoned);
    let queued = taken?;
    match queued {
        Some(message) => {
            // Delivery takes the message out of the inbox, so the sender's
            // user gets the queue charge back (issue #103).
            release_queued_quota(message.quota_uid, message.bytes.len());
            Ok(Some(deliver(message)?))
        }
        None => Ok(None),
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
        channel.txns[index].state = TxnState::TimedOut;
        channel.timeouts += 1;
        let caller = channel.txns[index].caller;
        release_pending(channel, caller);
        woken.push(caller);
    }
}

/// Install a queued message's transfers into the receiving task's handle table
/// and rewrite them to local numbers.
///
/// On failure (the receiver is out of handles) everything installed is rolled
/// back and the references of everything still pending are released, so a
/// failed delivery cannot leak handles or frames.
pub(super) fn deliver(queued: Queued) -> Result<Message, Error> {
    let mut handles_out: Vec<u64> = Vec::with_capacity(queued.handles.len());
    let mut buffers_out: Vec<BufferDesc> = Vec::with_capacity(queued.buffers.len());
    for (index, transfer) in queued.handles.iter().enumerate() {
        let opened = if transfer.kind == HandleKind::Buffer {
            shared::attach(transfer.object_id, transfer.rights).map_err(from_shared)
        } else {
            handles::open(transfer.kind, transfer.rights, transfer.object_id).map_err(from_handles)
        };
        match opened {
            Ok(handle) => handles_out.push(handle),
            Err(error) => {
                rollback_delivery(&queued, index, &handles_out, &buffers_out);
                return Err(error);
            }
        }
    }
    for buffer in queued.buffers.iter() {
        match shared::attach(buffer.object_id, buffer.rights) {
            Ok(handle) => buffers_out.push(BufferDesc {
                handle,
                offset: buffer.offset,
                len: buffer.len,
                flags: buffer.flags,
            }),
            Err(error) => {
                rollback_delivery(&queued, queued.handles.len(), &handles_out, &buffers_out);
                return Err(from_shared(error));
            }
        }
    }
    Ok(Message {
        sender: queued.sender,
        method: queued.method,
        flags: queued.flags,
        txn: queued.txn,
        deadline: queued.deadline,
        bytes: queued.bytes,
        handles: handles_out,
        buffers: buffers_out,
    })
}

/// Undo a partial [`deliver`]: close the installed handles and release the
/// message references of everything still pending.
///
/// Non-buffer objects have no kernel object refcount yet, so releasing a moved
/// handle whose delivery failed drops the handle but not the object; that is
/// the documented follow-up for when `HandleEntry` grows a refcount.
pub(super) fn rollback_delivery(
    queued: &Queued,
    installed: usize,
    handles_out: &[u64],
    buffers_out: &[BufferDesc],
) {
    for (transfer, &handle) in queued.handles[..installed].iter().zip(handles_out) {
        if transfer.kind == HandleKind::Buffer {
            shared::close(handle).ok();
        } else {
            handles::close(handle).ok();
        }
    }
    for transfer in &queued.handles[installed..] {
        if transfer.kind == HandleKind::Buffer {
            shared::release(transfer.object_id);
        }
    }
    for descriptor in buffers_out {
        // Closing the receiver's buffer handle drops the reference `attach`
        // converted from the message.
        shared::close(descriptor.handle).ok();
    }
    for buffer in &queued.buffers[buffers_out.len()..] {
        shared::release(buffer.object_id);
    }
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
    if !endpoint.waiters.contains(&slot) {
        endpoint.waiters.push(slot);
    }
}

/// Drop `slot`'s registration on one side, if the channel still exists.
fn remove_waiter(channel_id: u64, side: usize, slot: usize) {
    let mut channels = CHANNELS.lock();
    if let Ok(channel) = find_channel(&mut channels, channel_id) {
        channel.endpoints[side]
            .waiters
            .retain(|&waiter| waiter != slot);
    }
}

/// Wake each task in `slots` that is still parked on the Messenger queue,
/// returning the first one that actually woke.
///
/// Called with `CHANNELS` released (queue-then-task lock order). A slot that
/// already returned, timed out or parked elsewhere is skipped by
/// `notify_task`, so a stale registration can never wake an unrelated wait.
pub(super) fn wake(slots: &[usize]) -> Option<usize> {
    let mut first = None;
    for &slot in slots {
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
