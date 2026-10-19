//! Synchronous transactions: begin, await, reply, cancel (section 6).
//!
//! The `_owned` forms take the parcel bytes by value: the syscall layer
//! copies a parcel in once and the same buffer is queued, delivered and
//! copied out (P6.3), so a message crosses the kernel with one copy in and
//! one copy out. The slice forms copy once for kernel-internal callers.

use super::*;

/// Start a synchronous call (section 6): register a fresh `txn_id` and enqueue
/// the request. The caller stays runnable; [`await_reply`] is the half that
/// blocks until the transaction ends.
///
/// Returns the transaction id; the caller completes it with [`await_reply`].
/// A request that cannot even be queued (wrong handle, malformed parcel, full
/// queue, dead peer, nested cycle) fails before the transaction is created.
pub fn begin_call(
    handle: u64,
    method: u32,
    parcel_bytes: &[u8],
    deadline: Option<u64>,
) -> Result<u64, Error> {
    begin_call_owned(handle, method, parcel_bytes.to_vec(), deadline)
}

/// [`begin_call`] that queues `parcel_bytes` itself instead of a copy.
pub fn begin_call_owned(
    handle: u64,
    method: u32,
    parcel_bytes: Vec<u8>,
    deadline: Option<u64>,
) -> Result<u64, Error> {
    let me = task::current();
    let (channel_id, side) = endpoint_of(handle, rights::CALL)?;
    let parcel = validate_parcel(&parcel_bytes)?;
    let parcel_flags = parcel.header.flags;
    let objects = resolve_objects(&parcel)?;
    let numbers = moved_handles(&parcel);
    let peer = 1 - side;
    let txn_id = new_txn_id(channel_id);
    let receivers = {
        let mut channels = CHANNELS.lock();
        let channel = find_channel(&mut channels, channel_id)?;
        if channel.endpoints[peer].closed {
            return Err(Error::PeerDied);
        }
        if channel.endpoints[peer].kernel_held {
            return Err(Error::MissingRight);
        }
        if channel.endpoints[peer].inbox.len() >= MAX_QUEUE_DEPTH
            || channel.endpoints[peer]
                .queued_bytes
                .saturating_add(parcel_bytes.len())
                > MAX_QUEUE_BYTES
        {
            channel.drops += 1;
            return Err(Error::QueueFull);
        }
        let outstanding = channel
            .txns
            .iter()
            .filter(|txn| txn.state == TxnState::Pending)
            .count();
        if outstanding >= MAX_OUTSTANDING {
            channel.drops += 1;
            return Err(Error::TooManyOutstanding);
        }
        // A transaction holds its slot, and a reply up to a parcel long, until
        // its caller collects the outcome (`take_outcome`), not only while it
        // is `Pending`: a caller that never awaits must hit the cap too
        // (audit H9).
        let sender_holds = channel.txns.iter().filter(|txn| txn.caller == me).count() as u64;
        if sender_holds >= MAX_PENDING_PER_SENDER {
            channel.drops += 1;
            return Err(Error::Quota);
        }
        // Section 6: a synchronous call is a cycle, refused with ERR_DEADLOCK
        // unless ALLOW_NESTED opts out, when it is either
        // * nesting: this task already has a call open on the channel, or
        // * a callback: a call is open *toward* this side, so its caller is
        //   parked waiting for this side and cannot serve a call back.
        // Calls already open in the same direction by *other* tasks are not a
        // cycle: registry resolves alias one endpoint (`registry::resolve`),
        // so independent clients of a service share this channel and must be
        // able to call it concurrently; the service answers them in turn.
        let cycle = channel.txns.iter().any(|txn| {
            txn.state == TxnState::Pending && (txn.caller == me || txn.callee_side == side)
        });
        if cycle && parcel_flags & flags::ALLOW_NESTED == 0 {
            return Err(Error::Deadlock);
        }
        let queued_bytes = parcel_bytes.len();
        let queued = Queued {
            sender: me,
            origin: SenderId::of(me),
            method,
            flags: parcel_flags,
            txn: Some(txn_id),
            deadline,
            bytes: parcel_bytes,
            objects,
        };
        // Per-uid queue quota (issue #103), then take the buffer references and
        // finish the channel move before the request is visible, so a callee
        // that runs immediately finds the objects already in the message.
        if let Err(error) = charge_queued(queued.origin.uid, queued_bytes) {
            channel.drops += 1;
            return Err(error);
        }
        if let Err(error) = retain_objects(&queued) {
            release_queued_quota(queued.origin.uid, queued_bytes);
            channel.drops += 1;
            return Err(error);
        }
        close_moved_handles(&numbers);
        channel.txns.push(Transaction {
            id: txn_id,
            caller: me,
            caller_side: side,
            callee_side: peer,
            // A poll waits a bounded grace for the callee; see `POLL_DEADLINE`.
            deadline: if deadline == Some(POLL_DEADLINE) {
                Some(task::ticks() + POLL_GRACE_TICKS)
            } else {
                deadline
            },
            poll: deadline == Some(POLL_DEADLINE),
            served: false,
            receiver: None,
            state: TxnState::Pending,
            reply: Vec::new(),
            charge: None,
        });
        let endpoint = &mut channel.endpoints[peer];
        endpoint.inbox.push_back(queued);
        endpoint.queued_bytes += queued_bytes;
        endpoint.arrivals += 1;
        let receivers = endpoint.waiters.take();
        channel.calls += 1;
        let sender = meter(channel, me);
        sender.sent += 1;
        sender.calls += 1;
        sender.outstanding += 1;
        receivers
    };
    // Wake the callee: it may already be parked in `recv`, and nothing else
    // wakes it for this request. `send` does the same; the userspace
    // `messengerd` round trip depends on it. The caller is about to park in
    // `await_reply`: the callee runs in its place (P6.2).
    ring(channel_id, peer);
    hand_off_to(wake(receivers.iter()));
    // The caller stays runnable: `call` parks in `await_reply` (in the same
    // interrupts-off syscall, so no reply can slip in before the first wait),
    // and the two-step syscall form (`OP_CALL_BEGIN`, `OP_CALL_AWAIT`) must
    // return to user mode runnable. Parking here left a task that begins a
    // call and then serves its own request (messengerd's self-soak) marked
    // blocked in user mode; with targeted wakeups nothing else ever woke it,
    // so a timer tick in that window stranded it forever (issue #338).
    Ok(txn_id)
}

/// Block until `txn_id` ends, then return the reply or the failure.
///
/// Every wakeup is treated as advisory: the transaction is re-checked under
/// the registry lock and the caller parks again if the outcome is not terminal
/// yet. A `TimedOut` wake marks the transaction expired, so a reply arriving
/// after the deadline is refused instead of delivered. Only the transaction's
/// own task may await it ([`assert_caller`], issue #725 review): an id is a
/// global counter and a foreign task must never consume its reply.
pub fn await_reply(txn_id: u64) -> Result<Vec<u8>, Error> {
    let me = task::current();
    assert_caller(txn_id)?;
    loop {
        let deadline = match take_outcome(txn_id)? {
            Outcome::Done(outcome) => {
                // The terminal transition already woke us (or the event
                // arrived before the first wait); drop the reason so it
                // cannot become a spurious wakeup for the next blocking call.
                let _ = task::take_wake_reason(me);
                return outcome;
            }
            Outcome::Pending(deadline) => deadline,
        };
        // This is the only park of a call: `begin_call` left us runnable, and
        // the queue drops our entry when the wake is consumed.
        let reason = MESSENGER.wait(me, deadline);
        if reason == WakeReason::TimedOut {
            expire_transaction(txn_id);
        }
        // A killed caller must reach its syscall return to die: cancel the
        // call so the next check ends the wait (with `Canceled`).
        if reason == WakeReason::Interrupted && task::signal::killed(me) {
            let _ = cancel(txn_id);
        }
    }
}

/// Synchronous call convenience: [`begin_call`] plus [`await_reply`].
pub fn call(
    handle: u64,
    method: u32,
    parcel_bytes: &[u8],
    deadline: Option<u64>,
) -> Result<Vec<u8>, Error> {
    call_owned(handle, method, parcel_bytes.to_vec(), deadline)
}

/// [`call`] that queues `parcel_bytes` itself instead of a copy.
pub fn call_owned(
    handle: u64,
    method: u32,
    parcel_bytes: Vec<u8>,
    deadline: Option<u64>,
) -> Result<Vec<u8>, Error> {
    let txn_id = begin_call_owned(handle, method, parcel_bytes, deadline)?;
    await_reply(txn_id)
}

/// Answer a pending transaction with a reply parcel.
///
/// Replies may arrive out of order (they match by id), and a reply to an
/// expired, canceled, or dead transaction is refused and counted as a drop.
/// Only the task that received the request, or a task holding a handle with
/// `CALL` to the endpoint it arrived on, may answer: any other task gets
/// [`Error::NotReceiver`] and the transaction is untouched. Transaction ids
/// are a global counter, so an id is guessable and cannot authorize anything
/// ([`txn::assert_caller`] guards the awaiting side the same way).
pub fn reply(txn_id: u64, parcel_bytes: &[u8]) -> Result<(), Error> {
    reply_owned(txn_id, parcel_bytes.to_vec())
}

/// [`reply`] that stores `parcel_bytes` itself instead of a copy.
pub fn reply_owned(txn_id: u64, parcel_bytes: Vec<u8>) -> Result<(), Error> {
    let parcel = validate_parcel(&parcel_bytes)?;
    // A reply carries no objects (`docs/messenger-core-plan.md` 2.3): replies
    // travel back through `await_reply`, which returns bytes only.
    if parcel.object_count() != 0 {
        return Err(Error::UnsupportedTransfer);
    }
    let (caller, refusal) = {
        let mut channels = CHANNELS.lock();
        let (channel, index) = channels.txn_mut(txn_id).ok_or(Error::NoTransaction)?;
        // Before the state check, so a non-receiver is told the same thing
        // for a live call and a finished one, and not counted as a drop: it
        // would let any task skew a service channel's statistics. (An id
        // that names nothing still reads `NoTransaction`.)
        if !may_reply(channel, index, task::current()) {
            return Err(Error::NotReceiver);
        }
        if channel.txns[index].state != TxnState::Pending {
            channel.drops += 1;
            return Err(Error::NoTransaction);
        }
        let caller = channel.txns[index].caller;
        // The reply sits in the kernel heap until its caller collects it, so
        // it is charged to the caller's user (audit H9): a caller that never
        // collects can hold only its quota of them. One over the quota ends
        // the call instead (the caller is told it timed out) and the replier
        // is told why.
        let uid = credentials::of(caller).uid;
        let bytes = parcel_bytes.len() as u64;
        let refusal = match quota::charge(uid, Resource::QueueBytes, bytes) {
            Ok(()) => {
                channel.txns[index].state = TxnState::Replied;
                channel.txns[index].reply = parcel_bytes;
                channel.txns[index].charge = Some((uid, bytes));
                channel.replies += 1;
                release_pending(channel, caller);
                None
            }
            Err(_) => {
                channel.drops += 1;
                time_out(channel, index, false);
                Some(Error::QuotaExceeded)
            }
        };
        (caller, refusal)
    };
    // The completion queue learns of the end beside the wake: a loop that
    // parked in `wait` with `WAIT_CALLS` instead of `await_reply` resumes
    // from the doorbell and takes the reply through its own later click.
    completion::record(caller, txn_id);
    // Only the caller waits for this outcome (in `await_reply`, or still
    // parked by `begin_call`); it runs as soon as the replier parks (P6.2).
    hand_off_to(wake([caller]));
    match refusal {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Cancel a pending transaction. Only its caller may cancel; the wait in
/// [`await_reply`] ends with [`Error::Canceled`].
pub fn cancel(txn_id: u64) -> Result<(), Error> {
    let me = task::current();
    {
        let mut channels = CHANNELS.lock();
        let (channel, index) = channels.txn_mut(txn_id).ok_or(Error::NoTransaction)?;
        if channel.txns[index].caller != me {
            return Err(Error::NotCaller);
        }
        if channel.txns[index].state != TxnState::Pending {
            return Err(Error::NoTransaction);
        }
        channel.txns[index].state = TxnState::Canceled;
        channel.cancels += 1;
        release_pending(channel, me);
    }
    // The registry lock is released (like `reply` and the close paths), so
    // the recording (and the wake it may ring) cannot invert against it.
    // The canceller is the caller, which is running; `await_reply` sees the
    // Canceled outcome on its next check without any wake, and the
    // completion queue still records the end: an unparked caller's future
    // reports itself from the queue.
    completion::record(me, txn_id);
    Ok(())
}
