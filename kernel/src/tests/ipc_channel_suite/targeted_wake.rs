//! Targeted wakeups (issue #338): a Messenger operation wakes only the task
//! it concerns, never every parked receiver in the system.
//!
//! Receivers are parked with `channels::harness::park_receiver`, which
//! registers a slot exactly where `recv` does, without a context switch.

use super::*;
use channels::harness as chan;

fn parked(slot: usize) -> bool {
    matches!(task::harness::state(slot), Some(TaskState::Blocked { .. }))
}

fn runnable(slot: usize) -> bool {
    task::harness::state(slot) == Some(TaskState::Runnable)
}

/// `count` fresh channels plus one spawned receiver per channel, parked in
/// `recv` on the channel's second endpoint. Returns `(sender, receiver
/// handle, receiver slot)` triples; every handle lives in the kernel task.
fn parked_receivers(count: usize) -> Result<Vec<(u64, u64, usize)>, String> {
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let (sender, receiver) = channels::create().map_err(reason)?;
        task::harness::switch_current(task::KERNEL_TASK);
        let slot = task::spawn_fork().map_err(|error| format!("spawn {index}: {error}"))?;
        chan::park_receiver(slot, receiver, None).map_err(reason)?;
        out.push((sender, receiver, slot));
    }
    task::harness::switch_current(task::KERNEL_TASK);
    Ok(out)
}

/// Finish and reap every spawned slot, then reset the channel state.
fn teardown(slots: impl IntoIterator<Item = usize>) -> Result<(), String> {
    task::harness::switch_current(task::KERNEL_TASK);
    for slot in slots {
        channels::forget_task(slot);
        task::harness::finish(slot, 0);
    }
    while task::reap_child().is_some() {}
    fresh()
}

/// Check that exactly `woken` (if any) left the parked set.
fn only_woken(set: &[(u64, u64, usize)], woken: Option<usize>, what: &str) -> Result<(), String> {
    for &(_, _, slot) in set {
        if Some(slot) == woken {
            check!(
                runnable(slot) && task::harness::take_wake_reason(slot) == Some(WakeReason::Woken),
                "{what}: target {slot} was not woken: {:?}",
                task::harness::state(slot)
            );
        } else {
            check!(
                parked(slot),
                "{what}: bystander {slot} was woken: {:?}",
                task::harness::state(slot)
            );
        }
    }
    Ok(())
}

/// One `send` wakes exactly the receiver parked on the destination endpoint
/// and consumes its registration; the other receivers stay parked.
pub fn send_wakes_only_destination() -> Result<(), String> {
    fresh()?;
    let set = parked_receivers(6)?;
    let (sender, receiver, target) = set[3];
    channels::send(sender, &parcel(5, 0, "only you")?).map_err(reason)?;
    only_woken(&set, Some(target), "send")?;
    check!(
        chan::endpoint_waiters(receiver).map_err(reason)?.is_empty(),
        "the woken receiver is still registered"
    );
    check!(
        !chan::is_queued(target),
        "the woken receiver is still on the messenger queue"
    );
    let message = channels::try_recv(receiver)
        .map_err(reason)?
        .ok_or("the woken receiver found an empty inbox")?;
    check!(payload(&message.bytes)? == "only you", "wrong payload");
    check!(
        chan::total_waiters() == set.len() - 1,
        "registrations after one wake: {}",
        chan::total_waiters()
    );
    teardown(set.iter().map(|&(_, _, slot)| slot))
}

/// `begin_call` wakes only the callee's receiver, and the `reply` wakes only
/// the caller: no parked receiver sees either event.
pub fn reply_wakes_only_caller() -> Result<(), String> {
    fresh()?;
    let set = parked_receivers(4)?;
    let (client, server, callee) = set[1];
    let me = task::current();
    let txn =
        channels::begin_call(client, 7, &parcel(7, flags::SYNC, "q")?, None).map_err(reason)?;
    only_woken(&set, Some(callee), "begin_call")?;
    check!(runnable(me), "begin_call parked the caller");
    // Park the caller the way `await_reply` does, so the reply has someone
    // to wake.
    chan::park(me, None);
    check!(blocked_call(me, None), "the caller did not park");

    let request = channels::try_recv(server)
        .map_err(reason)?
        .ok_or("the callee found no request")?;
    // Re-park the callee so the reply has a bystander on the call's own
    // channel as well as on the others.
    chan::park_receiver(callee, server, None).map_err(reason)?;
    channels::reply(request.txn.ok_or("no txn")?, &parcel(8, 0, "a")?).map_err(reason)?;
    check!(
        runnable(me) && task::harness::take_wake_reason(me) == Some(WakeReason::Woken),
        "reply did not wake the caller: {:?}",
        task::harness::state(me)
    );
    only_woken(&set, None, "reply")?;
    let got = channels::await_reply(txn).map_err(reason)?;
    check!(payload(&got)? == "a", "wrong reply payload");
    teardown(set.iter().map(|&(_, _, slot)| slot))
}

/// Closing an endpoint wakes the receiver parked on its peer (which then
/// sees `PeerDied`) and nobody else.
pub fn peer_close_wakes_peer_waiter() -> Result<(), String> {
    fresh()?;
    let set = parked_receivers(5)?;
    let (sender, receiver, target) = set[2];
    channels::close_endpoint(sender).map_err(reason)?;
    only_woken(&set, Some(target), "close")?;
    check!(
        channels::try_recv(receiver) == Err(ChannelError::PeerDied),
        "the surviving side does not report PeerDied"
    );
    // A task that dies parked in `recv` leaves no registration behind.
    let (_, other, stale) = set[4];
    channels::forget_task(stale);
    check!(
        chan::endpoint_waiters(other).map_err(reason)?.is_empty(),
        "forget_task left a stale waiter registration"
    );
    teardown(set.iter().map(|&(_, _, slot)| slot))
}

/// Soak: 4 receivers parked on 4 channels, 20000 one-way sends spread over
/// them. Every send wakes exactly its destination (no lost and no spurious
/// wakeups), and neither the endpoint lists nor the messenger queue grow.
pub fn targeted_wake_soak() -> Result<(), String> {
    const RECEIVERS: usize = 4;
    const ROUNDS: usize = 20_000;
    fresh()?;
    let set = parked_receivers(RECEIVERS)?;
    let body = parcel(5, 0, "tick")?;
    // SAFETY: `rdtsc` only reads the time-stamp counter; no memory is touched.
    let start = unsafe { core::arch::x86_64::_rdtsc() };
    for round in 0..ROUNDS {
        let pick = (round * 7 + round / RECEIVERS) % RECEIVERS;
        let (sender, receiver, slot) = set[pick];
        channels::send(sender, &body).map_err(reason)?;
        for (index, &(_, _, other)) in set.iter().enumerate() {
            let expected = index == pick;
            check!(
                runnable(other) == expected,
                "round {round}: slot {other} runnable={} (target {slot})",
                runnable(other)
            );
        }
        let _ = task::harness::take_wake_reason(slot);
        check!(
            channels::try_recv(receiver).map_err(reason)?.is_some(),
            "round {round}: the woken receiver's message is missing"
        );
        chan::park_receiver(slot, receiver, None).map_err(reason)?;
        check!(
            chan::total_waiters() == RECEIVERS && chan::queued_waiters() == RECEIVERS,
            "round {round}: {} registrations, {} queue entries",
            chan::total_waiters(),
            chan::queued_waiters()
        );
    }
    // SAFETY: as above, a side-effect-free counter read.
    let cycles = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start);
    serial_println!(
        "TEST:ipc_channel_targeted_wake_soak:INFO:receivers={RECEIVERS} rounds={ROUNDS} cycles={cycles}"
    );
    let stats = channels::stats();
    check!(
        stats.queued == 0 && stats.drops == 0,
        "counters after the soak: {stats:?}"
    );
    // Peer death releases every registration and queue entry.
    for &(sender, _, _) in &set {
        channels::close_endpoint(sender).map_err(reason)?;
    }
    check!(
        chan::total_waiters() == 0 && chan::queued_waiters() == 0,
        "leaked {} registrations / {} queue entries after closing",
        chan::total_waiters(),
        chan::queued_waiters()
    );
    teardown(set.iter().map(|&(_, _, slot)| slot))
}

/// A task that begins a call and then serves that request itself (the shape
/// of `messengerd`'s self-soak over the bootstrap channel) never leaves
/// `begin_call` blocked: it stays runnable through the receive and the reply,
/// and `await_reply` returns the answer. With the caller parked by
/// `begin_call`, a timer tick between the begin and the reply stranded the
/// task in user mode, since only its own reply could wake it (issue #338).
pub fn self_call_stays_runnable() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let me = task::current();
    let txn =
        channels::begin_call(client, 7, &parcel(7, flags::SYNC, "soak")?, None).map_err(reason)?;
    check!(
        runnable(me),
        "begin_call parked the caller: {:?}",
        task::harness::state(me)
    );
    let request = channels::try_recv(server)
        .map_err(reason)?
        .ok_or("the caller could not receive its own request")?;
    check!(runnable(me), "recv of the own request blocked the caller");
    channels::reply(request.txn.ok_or("no txn")?, &parcel(8, 0, "ok")?).map_err(reason)?;
    check!(
        runnable(me) && task::harness::take_wake_reason(me).is_none(),
        "a self-reply left a wake reason or a blocked state: {:?}",
        task::harness::state(me)
    );
    let got = channels::await_reply(txn).map_err(reason)?;
    check!(payload(&got)? == "ok", "wrong self-call reply");
    fresh()
}

/// Soak: 5000 self-calls in a row, checking the caller is runnable after
/// every begin and that no transaction or wait-queue entry leaks.
pub fn self_call_soak() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let me = task::current();
    let request = parcel(7, flags::SYNC, "soak")?;
    let answer = parcel(8, 0, "ok")?;
    for round in 0..5000u32 {
        let txn = channels::begin_call(client, 7, &request, None).map_err(reason)?;
        check!(runnable(me), "round {round}: begin_call parked the caller");
        let message = channels::try_recv(server)
            .map_err(reason)?
            .ok_or("round lost its request")?;
        channels::reply(message.txn.ok_or("no txn")?, &answer).map_err(reason)?;
        let got = channels::await_reply(txn).map_err(reason)?;
        check!(payload(&got)? == "ok", "round {round}: wrong reply");
    }
    let stats = channels::stats();
    check!(stats.outstanding == 0, "transactions leaked: {stats:?}");
    check!(
        chan::queued_waiters() == 0,
        "wait-queue entries leaked: {}",
        chan::queued_waiters()
    );
    fresh()
}
