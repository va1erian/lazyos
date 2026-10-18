//! Messages the receiver took off the queue and could not use.
//!
//! A request that is dequeued is the only record of its transaction's
//! callee, so every way it can fail to be delivered must end the
//! transaction explicitly: left `Pending`, a caller without a deadline
//! blocks forever and the caller's per-channel cap fills up (issue
//! `docs/audit-core-2026-10.md` H2, M-K1).

use super::*;

/// End the pending transaction `txn_id` as `PeerDied`, the answer a caller
/// gets when nobody will serve its request, and wake its caller. A
/// transaction that already ended (a poll that expired meanwhile, a caller
/// that canceled) is left alone.
pub(in crate::ipc::channels) fn fail_request(txn_id: u64) {
    let caller = {
        let mut channels = CHANNELS.lock();
        let Some((channel, index)) = channels.txn_mut(txn_id) else {
            return;
        };
        if channel.txns[index].state != TxnState::Pending {
            return;
        }
        channel.txns[index].state = TxnState::PeerDied;
        channel.drops += 1;
        let caller = channel.txns[index].caller;
        release_pending(channel, caller);
        caller
    };
    // The registry lock is released, as for every terminal transition;
    // `record` wakes a caller parked in `await_reply` or a wait set.
    completion::record(caller, txn_id);
}

/// Drop a dequeued message that was never delivered: release the buffer
/// references it held, close the channel ends it was moving (their peers
/// learn), and fail its request.
pub(super) fn discard_undelivered(queued: &Queued) {
    rollback_delivery(queued, &[]);
    if let Some(txn) = queued.txn {
        fail_request(txn);
    }
}

/// Take back a message that was delivered but could not be handed to the
/// receiver (the copy-out failed): close the handles it installed, as
/// [`rollback_delivery`] does for a partial delivery, and fail its request.
pub fn undo_delivery(message: Message) {
    let mut orphans: Vec<u64> = Vec::new();
    for &handle in &message.objects {
        let Ok(entry) = handles::get(handle) else {
            continue;
        };
        match entry.kind {
            HandleKind::Channel => {
                handles::close(handle).ok();
                orphans.push(entry.object_id);
            }
            _ => {
                shared::close(handle).ok();
            }
        }
    }
    close_orphans(orphans);
    if let Some(txn) = message.txn {
        fail_request(txn);
    }
}
