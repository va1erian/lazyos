//! Transaction deadlines and expiry.

use super::*;

/// The deadline recorded for a transaction, if it is still outstanding.
pub(super) fn transaction_deadline(txn_id: u64) -> Result<Option<u64>, Error> {
    let channels = CHANNELS.lock();
    for channel in channels.iter() {
        if let Some(txn) = channel.txns.iter().find(|txn| txn.id == txn_id) {
            return Ok(txn.deadline);
        }
    }
    Err(Error::NoTransaction)
}

/// Remove and return a terminal transaction's outcome, or `Ok(None)` while it
/// is still pending.
pub(super) fn take_outcome(txn_id: u64) -> Result<Option<Result<Vec<u8>, Error>>, Error> {
    let mut channels = CHANNELS.lock();
    for channel in channels.iter_mut() {
        let Some(index) = channel.txns.iter().position(|txn| txn.id == txn_id) else {
            continue;
        };
        if channel.txns[index].state == TxnState::Pending {
            return Ok(None);
        }
        let txn = channel.txns.remove(index);
        let outcome = match txn.state {
            TxnState::Replied => Ok(txn.reply),
            TxnState::TimedOut => Err(Error::TimedOut),
            TxnState::Canceled => Err(Error::Canceled),
            TxnState::PeerDied => Err(Error::PeerDied),
            TxnState::Pending => Err(Error::NoTransaction),
        };
        return Ok(Some(outcome));
    }
    Err(Error::NoTransaction)
}

/// Mark a pending transaction expired. A no-op if it already has another
/// terminal outcome, so a reply racing the deadline wins, and a no-op if its
/// deadline is not actually due: the caller may have parked on an earlier
/// deadline that a poll's receipt has since replaced.
pub(super) fn expire_transaction(txn_id: u64) {
    let mut channels = CHANNELS.lock();
    for channel in channels.iter_mut() {
        let Some(index) = channel.txns.iter().position(|txn| txn.id == txn_id) else {
            continue;
        };
        if channel.txns[index].state != TxnState::Pending {
            return;
        }
        if channel.txns[index]
            .deadline
            .is_some_and(|deadline| deadline > task::ticks())
        {
            return;
        }
        channel.txns[index].state = TxnState::TimedOut;
        channel.timeouts += 1;
        let caller = channel.txns[index].caller;
        release_pending(channel, caller);
        return;
    }
}

/// Test hook (issue #62 harness): run the timer's deadline sweep and mark every
/// expired transaction `TimedOut`, exactly as the wait loop would after
/// `WaitQueue::wait` returned `WakeReason::TimedOut`. Compiled only for the
/// in-kernel suite.
#[cfg(lazyos_tests)]
pub fn expire_deadlines(now: u64) {
    task::harness::expire_deadlines(now);
    let mut channels = CHANNELS.lock();
    for channel in channels.iter_mut() {
        let mut released = Vec::new();
        for index in 0..channel.txns.len() {
            let txn = &mut channel.txns[index];
            if txn.state == TxnState::Pending
                && txn.deadline.is_some_and(|deadline| deadline <= now)
            {
                txn.state = TxnState::TimedOut;
                released.push(txn.caller);
            }
        }
        channel.timeouts += released.len() as u64;
        for caller in released {
            release_pending(channel, caller);
        }
    }
    drop(channels);
    MESSENGER.notify_all();
}
