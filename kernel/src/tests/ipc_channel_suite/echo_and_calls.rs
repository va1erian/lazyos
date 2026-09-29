//! Echo round-trip, one-way ordering and limits, and deadline
//! timeout/race handling for synchronous calls.

use super::*;

/// A synchronous call: the caller parks, the request keeps its bytes, the
/// reply wakes the caller, and the round trip returns the reply parcel.
pub fn echo_roundtrip() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let request = parcel(7, flags::SYNC, "ping")?;
    let start = unsafe { core::arch::x86_64::_rdtsc() };

    let txn = channels::begin_call(client, 7, &request, None).map_err(reason)?;
    let me = task::current();
    check!(
        task::harness::state(me) == Some(TaskState::Runnable),
        "begin_call parked the caller: {:?}",
        task::harness::state(me)
    );

    // The server side sees the request with its kernel metadata intact.
    let message = channels::recv(server, None).map_err(reason)?;
    check!(
        message.sender == me,
        "sender is {}, expected {me}",
        message.sender
    );
    check!(message.method == 7, "method is {}", message.method);
    check!(
        message.txn == Some(txn),
        "transaction id is {:?}",
        message.txn
    );
    check!(message.bytes == request, "request bytes changed in flight");
    check!(message.handles.is_empty(), "request transferred handles");
    check!(
        payload(&message.bytes)? == "ping",
        "request payload changed"
    );

    // The reply is a fresh parcel, matched by transaction id. Park the caller
    // the way `await_reply` does, so the reply has a waiter to wake.
    channels::harness::park(me, None);
    let reply = parcel(8, 0, "pong")?;
    channels::reply(txn, &reply).map_err(reason)?;
    check!(
        task::harness::state(me) == Some(TaskState::Runnable),
        "reply did not wake the caller: {:?}",
        task::harness::state(me)
    );
    check!(
        task::harness::take_wake_reason(me) == Some(WakeReason::Woken),
        "reply wake reason is not Woken"
    );
    let got = channels::await_reply(txn).map_err(reason)?;
    check!(got == reply, "reply bytes changed on the way back");
    check!(payload(&got)? == "pong", "reply payload changed");

    let cycles = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start);
    serial_println!("TEST:ipc_channel_echo_roundtrip:INFO:cycles={cycles}");
    let stats = channels::stats();
    check!(
        stats.calls == 1 && stats.replies == 1 && stats.timeouts == 0,
        "counters after one echo: {stats:?}"
    );
    check!(
        stats.queued == 0 && stats.queued_bytes == 0 && stats.outstanding == 0,
        "channel not drained: {stats:?}"
    );
    let senders = channels::senders(client).map_err(reason)?;
    check!(
        senders.len() == 1
            && senders[0].slot == me
            && senders[0].calls == 1
            && senders[0].sent == 1
            && senders[0].outstanding == 0,
        "sender metering is {senders:?}"
    );
    fresh()
}

/// One-way sends enqueue in order, never park the sender, and are refused
/// with a metered drop when the peer's bounded queue is full.
pub fn one_way_order_and_limits() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    for index in 0..3u32 {
        let bytes = parcel(index, flags::ONE_WAY, &format!("m{index}"))?;
        channels::send(client, &bytes).map_err(reason)?;
    }
    let me = task::current();
    check!(
        task::harness::state(me) == Some(TaskState::Runnable),
        "send parked the sender: {:?}",
        task::harness::state(me)
    );
    for index in 0..3u32 {
        let message = channels::try_recv(server)
            .map_err(reason)?
            .ok_or("queued one-way message is missing")?;
        check!(
            message.txn.is_none(),
            "one-way message carries a transaction: {:?}",
            message.txn
        );
        check!(
            message.method == index,
            "order broken: method {}",
            message.method
        );
        check!(
            payload(&message.bytes)? == format!("m{index}"),
            "payload order broken"
        );
    }
    check!(
        channels::try_recv(server).map_err(reason)?.is_none(),
        "recv did not drain the queue"
    );

    // Fill the bounded queue, then observe the refusal and the drop meter.
    let bytes = parcel(0, flags::ONE_WAY, "fill")?;
    for _ in 0..channels::MAX_QUEUE_DEPTH {
        channels::send(client, &bytes).map_err(reason)?;
    }
    check!(
        channels::send(client, &bytes) == Err(ChannelError::QueueFull),
        "an overfull queue accepted a message"
    );
    let stats = channels::channel_stats(client).map_err(reason)?;
    check!(
        stats.drops == 1,
        "queue-full drop was not counted: {stats:?}"
    );
    check!(
        stats.queued == channels::MAX_QUEUE_DEPTH as u64,
        "queued depth is {}",
        stats.queued
    );
    let senders = channels::senders(server).map_err(reason)?;
    check!(
        senders.len() == 1 && senders[0].sent == 3 + channels::MAX_QUEUE_DEPTH as u64,
        "sender metering is {senders:?}"
    );
    fresh()
}

/// A call past its deadline wakes with `TimedOut`, and a late reply is
/// refused rather than delivered.
pub fn deadline_timeout() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let request = parcel(7, flags::SYNC, "slow")?;
    let me = task::current();
    let deadline = task::ticks() + 10;
    let txn = channels::begin_call(client, 7, &request, Some(deadline)).map_err(reason)?;
    // The caller stays runnable until `await_reply` parks it with the
    // transaction's deadline; the sweep expires the transaction itself.
    check!(
        task::harness::state(me) == Some(TaskState::Runnable),
        "begin_call parked the caller: {:?}",
        task::harness::state(me)
    );

    channels::expire_deadlines(deadline);
    let late = parcel(8, 0, "too late")?;
    check!(
        channels::reply(txn, &late) == Err(ChannelError::NoTransaction),
        "a reply to an expired transaction was accepted"
    );
    check!(
        channels::await_reply(txn) == Err(ChannelError::TimedOut),
        "await_reply did not report TimedOut"
    );
    let stats = channels::stats();
    check!(
        stats.timeouts == 1 && stats.outstanding == 0,
        "counters after a timeout: {stats:?}"
    );
    // The request stays queued for the (late) server to drain.
    check!(
        channels::try_recv(server).map_err(reason)?.is_some(),
        "the expired request vanished from the server queue"
    );
    fresh()
}

/// A reply that lands before the deadline sweep wins the race: the
/// transaction completes normally and the timeout meter stays at zero.
pub fn deadline_reply_race() -> Result<(), String> {
    fresh()?;
    let (client, _server) = channels::create().map_err(reason)?;
    let request = parcel(7, flags::SYNC, "fast")?;
    let deadline = task::ticks() + 10;
    let txn = channels::begin_call(client, 7, &request, Some(deadline)).map_err(reason)?;
    let reply = parcel(8, 0, "quick")?;
    channels::reply(txn, &reply).map_err(reason)?;
    // The sweep runs after the reply; it must not overwrite the outcome.
    channels::expire_deadlines(deadline);
    let got = channels::await_reply(txn).map_err(reason)?;
    check!(got == reply, "the racing reply was not returned");
    check!(
        channels::stats().timeouts == 0,
        "a completed reply was counted as timed out"
    );
    fresh()
}

/// The production `call` path, end to end: with an already-expired
/// deadline the caller parks through the timer gate, the deadline sweep
/// wakes it, and `call` returns `TimedOut` without a server.
pub fn call_deadline_zero() -> Result<(), String> {
    fresh()?;
    let (client, _server) = channels::create().map_err(reason)?;
    let request = parcel(7, flags::SYNC, "nobody home")?;
    let start = unsafe { core::arch::x86_64::_rdtsc() };
    let result = channels::call(client, 7, &request, Some(0));
    let cycles = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start);
    check!(
        result == Err(ChannelError::TimedOut),
        "an already-expired call returned {result:?}"
    );
    serial_println!("TEST:ipc_channel_call_deadline_zero:INFO:cycles={cycles}");
    let stats = channels::stats();
    check!(
        stats.timeouts == 1 && stats.outstanding == 0,
        "counters after a timeout: {stats:?}"
    );
    check!(
        task::harness::state(task::current()) == Some(TaskState::Runnable),
        "the caller stayed parked after call returned"
    );
    fresh()
}
