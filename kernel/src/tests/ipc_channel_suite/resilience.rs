//! Cancellation, peer death, deadlock refusal, and concurrent
//! clients sharing one endpoint (including a soak).

use super::*;

/// `cancel` (issued by the running caller between `begin_call` and
/// `await_reply`) ends the transaction with `Canceled`, leaves the caller
/// runnable with no stray wake reason, and the transaction is gone afterwards.
pub fn cancel_wakes() -> Result<(), String> {
    fresh()?;
    let (client, _server) = channels::create().map_err(reason)?;
    let request = parcel(7, flags::SYNC, "wait")?;
    let me = task::current();
    let txn = channels::begin_call(client, 7, &request, None).map_err(reason)?;
    check!(
        task::harness::state(me) == Some(TaskState::Runnable),
        "begin_call parked the caller: {:?}",
        task::harness::state(me)
    );
    channels::cancel(txn).map_err(reason)?;
    check!(
        task::harness::state(me) == Some(TaskState::Runnable)
            && task::harness::take_wake_reason(me).is_none(),
        "cancel blocked the caller or left a wake reason: {:?}",
        task::harness::state(me)
    );
    check!(
        channels::await_reply(txn) == Err(ChannelError::Canceled),
        "await_reply did not report Canceled"
    );
    check!(
        channels::cancel(txn) == Err(ChannelError::NoTransaction),
        "double cancel succeeded"
    );
    check!(
        channels::stats().cancels == 1,
        "cancel counter is {}",
        channels::stats().cancels
    );
    fresh()
}

/// Closing an endpoint wakes an outstanding caller with `PeerDied`, and the
/// surviving side sees `PeerDied` once its inbox is empty.
pub fn peer_died() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let request = parcel(7, flags::SYNC, "hello?")?;
    let me = task::current();
    let txn = channels::begin_call(client, 7, &request, None).map_err(reason)?;
    // Parked as `await_reply` would be, so the close has a caller to wake.
    channels::harness::park(me, None);
    channels::close_endpoint(server).map_err(reason)?;
    check!(
        task::harness::state(me) == Some(TaskState::Runnable),
        "close did not wake the caller: {:?}",
        task::harness::state(me)
    );
    check!(
        task::harness::take_wake_reason(me) == Some(WakeReason::Woken),
        "close wake reason is not Woken"
    );
    check!(
        channels::await_reply(txn) == Err(ChannelError::PeerDied),
        "await_reply did not report PeerDied"
    );
    check!(
        channels::recv(client, None) == Err(ChannelError::PeerDied),
        "recv did not report PeerDied after the peer closed"
    );
    check!(
        channels::stats().outstanding == 0,
        "transaction stayed outstanding after the peer died"
    );
    fresh()
}

/// A synchronous call while another transaction is open on the channel is
/// a cycle and refused with `Deadlock`; `ALLOW_NESTED` opts out, and the
/// channel is usable again once the first transaction ends.
pub fn deadlock_refused() -> Result<(), String> {
    fresh()?;
    let (a, b) = channels::create().map_err(reason)?;
    let request = parcel(7, flags::SYNC, "outer")?;
    let outer = channels::begin_call(a, 7, &request, None).map_err(reason)?;

    check!(
        channels::begin_call(b, 7, &request, None) == Err(ChannelError::Deadlock),
        "a nested call cycle was not refused"
    );
    let nested_bytes = parcel(7, flags::SYNC | flags::ALLOW_NESTED, "nested")?;
    let nested = channels::begin_call(b, 7, &nested_bytes, None).map_err(reason)?;
    check!(nested != outer, "the nested call reused the outer id");

    channels::cancel(outer).map_err(reason)?;
    channels::cancel(nested).map_err(reason)?;
    check!(
        channels::await_reply(outer) == Err(ChannelError::Canceled),
        "outer outcome is not Canceled"
    );
    check!(
        channels::await_reply(nested) == Err(ChannelError::Canceled),
        "nested outcome is not Canceled"
    );

    // With the channel idle again, a plain call is allowed.
    let again = channels::begin_call(a, 7, &request, None).map_err(reason)?;
    channels::cancel(again).map_err(reason)?;
    check!(
        channels::await_reply(again) == Err(ChannelError::Canceled),
        "reused channel outcome is not Canceled"
    );
    check!(
        channels::stats().cancels == 3,
        "cancel counter is {}",
        channels::stats().cancels
    );
    fresh()
}

/// Independent clients calling one service over an aliased endpoint are
/// not a cycle (the clipboard demo pair hit a false `Deadlock` here): the
/// second client's call is queued behind the first. Nesting by the same
/// client and a callback from the service side stay refused while either
/// call is open, and the callback is allowed once both calls end.
pub fn concurrent_clients_allowed() -> Result<(), String> {
    fresh()?;
    let (shared, server) = channels::create().map_err(reason)?;
    let clients = shared_clients(shared, 2)?;
    let (first, first_handle) = clients[0];
    let (second, second_handle) = clients[1];
    let request = parcel(7, flags::SYNC, "hello")?;

    task::harness::switch_current(first);
    let first_txn = channels::begin_call(first_handle, 7, &request, None).map_err(reason)?;
    task::harness::switch_current(second);
    let second_txn = channels::begin_call(second_handle, 7, &request, None)
        .map_err(|error| format!("second client refused: {}", error.message()))?;
    check!(first_txn != second_txn, "the two clients share a txn id");

    // Nesting: the first client already has a call open on this channel.
    task::harness::switch_current(first);
    check!(
        channels::begin_call(first_handle, 7, &request, None) == Err(ChannelError::Deadlock),
        "a nested call by the same client was not refused"
    );
    // Callback: the service calls toward the side whose callers are parked.
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        channels::begin_call(server, 7, &request, None) == Err(ChannelError::Deadlock),
        "a callback toward parked callers was not refused"
    );

    // The service answers both, in arrival order, by transaction id.
    for (slot, txn) in [(first, first_txn), (second, second_txn)] {
        let message = channels::recv(server, None).map_err(reason)?;
        check!(
            message.sender == slot && message.txn == Some(txn),
            "request from {} txn {:?}, expected {slot} txn {txn}",
            message.sender,
            message.txn
        );
        channels::reply(txn, &parcel(8, 0, &format!("to {slot}"))?).map_err(reason)?;
    }
    for (slot, txn) in [(first, first_txn), (second, second_txn)] {
        task::harness::switch_current(slot);
        let got = channels::await_reply(txn).map_err(reason)?;
        check!(
            payload(&got)? == format!("to {slot}"),
            "client {slot} got another client's reply"
        );
    }

    // Idle again: the service may now call its clients' side.
    task::harness::switch_current(task::KERNEL_TASK);
    let back = channels::begin_call(server, 7, &request, None).map_err(reason)?;
    channels::cancel(back).map_err(reason)?;
    check!(
        channels::await_reply(back) == Err(ChannelError::Canceled),
        "callback outcome is not Canceled"
    );
    // Canceling does not dequeue: the callback request is still waiting in
    // the clients' inbox.
    let stats = channels::stats();
    check!(
        stats.calls == 3 && stats.replies == 2 && stats.outstanding == 0 && stats.queued == 1,
        "counters after two clients and a callback: {stats:?}"
    );
    fresh()
}

/// Soak: 4 clients call one shared endpoint concurrently for 2000 rounds.
/// Every round the callback probe is refused, replies go back in reverse
/// order and each reaches its own caller; nothing is left outstanding,
/// queued or metered at the end.
pub fn concurrent_clients_soak() -> Result<(), String> {
    const CLIENTS: usize = 4;
    const ROUNDS: usize = 2000;
    fresh()?;
    let (shared, server) = channels::create().map_err(reason)?;
    let clients = shared_clients(shared, CLIENTS)?;
    let probe = parcel(7, flags::SYNC, "probe")?;
    let start = unsafe { core::arch::x86_64::_rdtsc() };
    for round in 0..ROUNDS {
        let mut txns = Vec::with_capacity(CLIENTS);
        for &(slot, handle) in &clients {
            task::harness::switch_current(slot);
            let request = parcel(7, flags::SYNC, &format!("{round}:{slot}"))?;
            let txn = channels::begin_call(handle, 7, &request, None)
                .map_err(|error| format!("round {round} client {slot}: {}", error.message()))?;
            txns.push(txn);
        }
        task::harness::switch_current(task::KERNEL_TASK);
        check!(
            channels::begin_call(server, 7, &probe, None) == Err(ChannelError::Deadlock),
            "round {round}: a callback was allowed with calls open"
        );
        let mut inbound = Vec::with_capacity(CLIENTS);
        for _ in 0..CLIENTS {
            inbound.push(channels::recv(server, None).map_err(reason)?);
        }
        for message in inbound.iter().rev() {
            let txn = message
                .txn
                .ok_or_else(|| format!("round {round}: a call arrived without a txn"))?;
            let echo = payload(&message.bytes)?;
            channels::reply(txn, &parcel(8, 0, &echo)?).map_err(reason)?;
        }
        for (index, &(slot, _)) in clients.iter().enumerate() {
            task::harness::switch_current(slot);
            let got = channels::await_reply(txns[index]).map_err(reason)?;
            check!(
                payload(&got)? == format!("{round}:{slot}"),
                "round {round}: client {slot} got the wrong reply"
            );
        }
    }
    task::harness::switch_current(task::KERNEL_TASK);
    let cycles = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start);
    serial_println!(
        "TEST:ipc_channel_concurrent_clients_soak:INFO:clients={CLIENTS} rounds={ROUNDS} cycles={cycles}"
    );
    let stats = channels::stats();
    let expected = (CLIENTS * ROUNDS) as u64;
    check!(
        stats.calls == expected
            && stats.replies == expected
            && stats.timeouts == 0
            && stats.outstanding == 0
            && stats.queued == 0
            && stats.queued_bytes == 0,
        "counters after the soak: {stats:?}"
    );
    let meters = channels::senders(server).map_err(reason)?;
    check!(
        meters.iter().all(|meter| meter.outstanding == 0),
        "a sender meter still counts open calls: {meters:?}"
    );
    fresh()
}
