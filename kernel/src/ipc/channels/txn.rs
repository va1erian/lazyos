//! Transaction deadlines and expiry.

use super::*;

/// Where a transaction stands for its waiting caller.
pub(super) enum Outcome {
    /// Ended: the reply, or why there is none. The transaction is gone.
    Done(Result<Vec<u8>, Error>),
    /// Still pending, with its current deadline.
    Pending(Option<u64>),
}

/// Remove and return a terminal transaction's outcome, or its deadline while
/// it is still pending: one indexed lookup (P6.3).
pub(super) fn take_outcome(txn_id: u64) -> Result<Outcome, Error> {
    let mut channels = CHANNELS.lock();
    let (channel, index) = channels.txn_mut(txn_id).ok_or(Error::NoTransaction)?;
    if channel.txns[index].state == TxnState::Pending {
        return Ok(Outcome::Pending(channel.txns[index].deadline));
    }
    let txn = channel.txns.swap_remove(index);
    Ok(Outcome::Done(match txn.state {
        TxnState::Replied => Ok(txn.reply),
        TxnState::TimedOut => Err(Error::TimedOut),
        TxnState::Canceled => Err(Error::Canceled),
        TxnState::PeerDied => Err(Error::PeerDied),
        TxnState::Pending => Err(Error::NoTransaction),
    }))
}

/// Mark a pending transaction expired. A no-op if it already has another
/// terminal outcome, so a reply racing the deadline wins, and a no-op if its
/// deadline is not actually due: the caller may have parked on an earlier
/// deadline that a poll's receipt has since replaced.
pub(super) fn expire_transaction(txn_id: u64) {
    let mut channels = CHANNELS.lock();
    let Some((channel, index)) = channels.txn_mut(txn_id) else {
        return;
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
