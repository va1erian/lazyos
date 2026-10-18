//! Channel ends and transaction outcomes nobody can reach any more, each
//! leak looped more times than its table holds (audit H8, H9): an end moved
//! into its own inbox, rings of channels carrying each other's ends, and
//! replies nobody collects. Every test ends with the tables empty.

use super::dropped::tables_empty;
use super::*;
use crate::tests::ipc_shared_suite::{parcel_with_objects, reap};
use alloc::vec;
use libmessenger::Object;

/// Send `payload_end` over `via`.
fn send_end(via: u64, payload_end: u64) -> Result<(), String> {
    let bytes = parcel_with_objects(1, "end", vec![Object::Channel(payload_end)])?;
    channels::send(via, &bytes).map_err(reason)
}

/// A channel end moved into its own channel's inbox is reclaimed when the
/// other end closes: 300 rounds, more than the 64 slots, end with no channel
/// left (H8: this ran out of slots after 64).
pub fn self_moved_end_reclaimed() -> Result<(), String> {
    fresh()?;
    for round in 0..300u32 {
        let (a0, a1) =
            channels::create().map_err(|error| format!("round {round}: {}", error.message()))?;
        send_end(a0, a1)?;
        channels::close_endpoint(a0).map_err(reason)?;
        check!(
            channels::counts().channels == 0,
            "round {round}: the channel outlived its last handle"
        );
    }
    fresh()
}

/// Two channels that carry each other's ends, closed from the outside: 150
/// rounds leave nothing behind, and the end that is still held learns its
/// peer is gone.
pub fn moved_end_ring_reclaimed() -> Result<(), String> {
    fresh()?;
    for round in 0..150u32 {
        let (a0, a1) =
            channels::create().map_err(|error| format!("round {round}: {}", error.message()))?;
        let (b0, b1) =
            channels::create().map_err(|error| format!("round {round}: {}", error.message()))?;
        send_end(b0, a1)?;
        send_end(a0, b1)?;
        channels::close_endpoint(a0).map_err(reason)?;
        check!(
            channels::try_recv(b0) == Err(ChannelError::PeerDied),
            "round {round}: the held end of the ring was not told"
        );
        channels::close_endpoint(b0).map_err(reason)?;
        check!(
            channels::counts().channels == 0,
            "round {round}: {} channels left",
            channels::counts().channels
        );
    }
    fresh()
}

/// The reclaimer must not touch an end that is only in flight: a message
/// waiting for a live receiver still carries a usable end, down a chain of
/// moves.
pub fn reclaim_spares_ends_in_flight() -> Result<(), String> {
    fresh()?;
    // A service pair whose server end is held, a pair whose end travels over
    // it, and a pair whose end travels over the travelling pair.
    let (service, service_server) = channels::create().map_err(reason)?;
    let (p0, p1) = channels::create().map_err(reason)?;
    let (q0, q1) = channels::create().map_err(reason)?;
    send_end(p0, q0)?;
    send_end(service, p1)?;
    // Closing p0 asks whether p1 is still reachable: it rides to the service.
    channels::close_endpoint(p0).map_err(reason)?;
    check!(
        channels::counts().channels == 3,
        "a live end was reclaimed: {} channels",
        channels::counts().channels
    );
    let message = channels::recv(service_server, None).map_err(reason)?;
    let p1 = *message
        .objects
        .first()
        .ok_or("the moved end never arrived")?;
    let inner = channels::recv(p1, None).map_err(reason)?;
    let q0 = *inner.objects.first().ok_or("the inner end never arrived")?;
    channels::close_endpoint(q0).map_err(reason)?;
    check!(
        channels::try_recv(q1) == Err(ChannelError::PeerDied),
        "the end that made the whole trip is dead"
    );
    fresh()
}

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

/// Moving both ends of a pair into the pair's own inbox closes no handle, so
/// no close triggers the reclaimer: only the full registry does. 200 rounds
/// (the table holds 64) keep succeeding, and an interrupt channel whose
/// sending side only the kernel holds survives every pass.
pub fn full_registry_reclaims_and_spares_kernel_sides() -> Result<(), String> {
    fresh()?;
    let (_, irq) = channels::create_irq_channel().map_err(reason)?;
    for round in 0..200u32 {
        let (a0, a1) = channels::create()
            .map_err(|error| format!("round {round}: create: {}", error.message()))?;
        send_end(a0, a1)?;
        send_end(a0, a0)?;
        check!(
            channels::counts().channels <= channels::MAX_CHANNELS as u64,
            "round {round}: {} channels",
            channels::counts().channels
        );
    }
    check!(
        channels::try_recv(irq) == Ok(None),
        "a reclaim pass closed the interrupt channel's kernel side"
    );
    fresh()
}
