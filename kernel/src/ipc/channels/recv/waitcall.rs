//! The items of a [`super::wait_any`] set: endpoints and pending calls
//! (issue #309; design note: docs/architecture/wait-any.md).
//!
//! A word of the set is an endpoint handle, or, with [`WAIT_ITEM_CALL`] set,
//! the id of a transaction the caller began with `begin_call` and has not
//! awaited yet. A call is ready once its transaction is terminal (replied,
//! timed out, canceled, or its peer died), so the caller's `await_reply`
//! returns at once. A call needs no per-object waiter list: every terminal
//! transition already wakes the transaction's caller on the Messenger queue
//! (`reply`, peer close, the caller's own cancel), which is the queue
//! `wait_any` parks on. Its deadline does need the wait: `await_reply` is what
//! expires a transaction when its caller's park times out, so the wait parks
//! no longer than the earliest pending call's deadline and expires the due
//! ones itself.

use super::*;

/// Marks a word of the wait set as a pending call's transaction id instead
/// of an endpoint handle. Transaction ids are a sequence number above the
/// registry slot and never reach bit 63; handle numbers are small.
pub const WAIT_ITEM_CALL: u64 = 1 << 63;

/// One resolved item of a wait set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Item {
    /// An endpoint: its channel id and side.
    Endpoint(u64, usize),
    /// A transaction `me` began and has not awaited.
    Call(u64),
}

impl Item {
    /// A placeholder for the fixed-size item array.
    pub(super) const NONE: Item = Item::Call(0);
}

/// Resolve one word of the set for task `me`. An endpoint needs `CALL`
/// rights, as `recv` does; a call must exist and belong to `me`
/// (`NoTransaction`, `NotCaller`), so a task can never observe someone
/// else's transaction.
pub(super) fn resolve_item(word: u64, me: usize) -> Result<Item, Error> {
    if word & WAIT_ITEM_CALL == 0 {
        let (id, side) = endpoint_of(word, rights::CALL)?;
        return Ok(Item::Endpoint(id, side));
    }
    let txn_id = word & !WAIT_ITEM_CALL;
    let mut channels = CHANNELS.lock();
    let (channel, index) = channels.txn_mut(txn_id).ok_or(Error::NoTransaction)?;
    if channel.txns[index].caller != me {
        return Err(Error::NotCaller);
    }
    Ok(Item::Call(txn_id))
}

/// Whether call `txn_id` is ready (terminal or gone), and if not its
/// deadline in ticks. Runs under the caller's `CHANNELS` lock.
pub(super) fn call_state(channels: &mut Registry, txn_id: u64) -> Result<(), Option<u64>> {
    match channels.txn_mut(txn_id) {
        Some((channel, index)) if channel.txns[index].state == TxnState::Pending => {
            Err(channel.txns[index].deadline)
        }
        _ => Ok(()),
    }
}

/// Expire every call of `items` whose deadline is due (after a timed-out
/// park), exactly as `await_reply` does for its own transaction.
pub(super) fn expire_due_calls(items: &[Item]) {
    for item in items {
        if let Item::Call(txn_id) = *item {
            expire_transaction(txn_id);
        }
    }
}

/// The earlier of two optional nanosecond deadlines (`None` is "never").
pub(super) fn earlier(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) => a,
        (None, b) => b,
    }
}
