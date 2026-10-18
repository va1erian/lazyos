//! Channel ends and transaction outcomes nobody can reach any more, each
//! leak looped more times than its table holds (audit H8, H9): an end moved
//! into its own inbox, rings of channels carrying each other's ends, and
//! replies nobody collects. Every test ends with the tables empty.

use super::dropped::tables_empty;
use super::*;
use crate::tests::ipc_shared_suite::{parcel_with_objects, reap};
use alloc::vec;
use libmessenger::Object;

/// Transactions whose outcome nobody collects count against their caller:
/// after `MAX_PENDING_PER_SENDER` unawaited replies the next call is refused
/// with `Quota`, and collecting one makes room again (H9).
pub fn uncollected_outcomes_count() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let request = parcel(1, flags::SYNC, "ping")?;
    let answer = parcel(1, 0, "pong")?;
    let mut held = Vec::new();
    for round in 0..channels::MAX_PENDING_PER_SENDER {
        let txn = channels::begin_call(client, 1, &request, None).map_err(reason)?;
        let message = channels::recv(server, None).map_err(reason)?;
        check!(message.txn == Some(txn), "round {round}: wrong request");
        channels::reply(txn, &answer).map_err(reason)?;
        held.push(txn);
    }
    check!(
        channels::begin_call(client, 1, &request, None) == Err(ChannelError::Quota),
        "a caller with {} uncollected replies made another call",
        held.len()
    );
    channels::await_reply(held.remove(0)).map_err(reason)?;
    let txn = channels::begin_call(client, 1, &request, None).map_err(reason)?;
    channels::cancel(txn).map_err(reason)?;
    held.push(txn);
    for txn in held {
        let _ = channels::await_reply(txn);
    }
    channels::reset();
    tables_empty("after collecting")?;
    fresh()
}

/// 60 callers that each leave a full quota of replies uncollected and exit:
/// the table is empty afterwards (it held 960 replies otherwise).
pub fn uncollected_outcomes_soak() -> Result<(), String> {
    fresh()?;
    let (shared_end, server) = channels::create().map_err(reason)?;
    let request = parcel(1, flags::SYNC, "ping")?;
    let answer = parcel(1, 0, "pong")?;
    for round in 0..60u32 {
        let clients = shared_clients(shared_end, 1)?;
        let (slot, handle) = clients[0];
        for _ in 0..channels::MAX_PENDING_PER_SENDER {
            task::harness::switch_current(slot);
            let txn = channels::begin_call(handle, 1, &request, None)
                .map_err(|error| format!("round {round}: {}", error.message()))?;
            task::harness::switch_current(task::KERNEL_TASK);
            let message = channels::recv(server, None).map_err(reason)?;
            check!(message.txn == Some(txn), "round {round}: wrong request");
            channels::reply(txn, &answer).map_err(reason)?;
        }
        task::harness::switch_current(slot);
        check!(
            channels::begin_call(handle, 1, &request, None) == Err(ChannelError::Quota),
            "round {round}: the caller was not capped"
        );
        task::harness::switch_current(task::KERNEL_TASK);
        channels::forget_task(slot);
        reap(slot)?;
        task::harness::switch_current(task::KERNEL_TASK);
        check!(
            channels::harness::live_transactions() == 0,
            "round {round}: {} transactions left",
            channels::harness::live_transactions()
        );
    }
    fresh()
}
