//! Who may answer a request (audit H1/M-K2): only the task that received it
//! or a holder of the callee endpoint, never a caller who guessed its id.

use super::*;
use crate::ipc::handles::rights;

/// A fresh task with an empty handle table.
fn spawn_task(label: &str) -> Result<usize, String> {
    task::harness::switch_current(task::KERNEL_TASK);
    let slot = task::spawn_fork().map_err(|error| format!("{label}: {error}"))?;
    handles::reset_for_task(slot);
    Ok(slot)
}

/// A task that holds `rights` to the same endpoint side as `handle` (a
/// handle in the current task's table), as name resolution would give it.
fn spawn_holder(label: &str, handle: u64, rights: u32) -> Result<usize, String> {
    let entry = handles::get(handle).map_err(|error| error.message())?;
    let slot = spawn_task(label)?;
    handles::open_for_task(slot, entry.kind, rights, entry.object_id)
        .map_err(|error| error.message())?;
    Ok(slot)
}

/// Attempt a reply as `slot`, restoring the kernel task afterwards.
fn reply_as(slot: usize, txn: u64, text: &str) -> Result<Result<(), ChannelError>, String> {
    let bytes = parcel(8, 0, text)?;
    task::harness::switch_current(slot);
    let outcome = channels::reply(txn, &bytes);
    task::harness::switch_current(task::KERNEL_TASK);
    Ok(outcome)
}

/// A caller, a client holding only the caller side, a holder of the callee
/// side without `CALL`, and a bystander cannot answer; the receiver, and a
/// worker holding the callee side, can. A refused forgery leaves the call
/// open for the real answer and does not count as a drop.
pub fn reply_refuses_non_receivers() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let request = parcel(7, flags::SYNC, "who?")?;
    let txn = channels::begin_call(client, 7, &request, None).map_err(reason)?;
    // Still queued: nobody has received it, and the kernel task holds both
    // sides, so only a task holding neither is refused here.
    let bystander = spawn_task("bystander")?;
    let client_only = spawn_holder("client", client, rights::ALL)?;
    let no_call = spawn_holder("no call right", server, rights::MONITOR)?;
    let worker = spawn_holder("worker", server, rights::CALL)?;
    task::harness::switch_current(task::KERNEL_TASK);
    let message = channels::recv(server, None).map_err(reason)?;
    check!(message.txn == Some(txn), "recv returned another txn");
    let drops = channels::stats().drops;
    for (label, slot) in [
        ("bystander", bystander),
        ("client-side holder", client_only),
        ("holder without CALL", no_call),
    ] {
        let outcome = reply_as(slot, txn, "forged")?;
        check!(
            outcome == Err(ChannelError::NotReceiver),
            "{label} was not refused: {outcome:?}"
        );
    }
    // Neighbouring ids: the same shape, never a transaction of this call.
    for step in 1..8u64 {
        let guess = txn.wrapping_add(step * channels::MAX_CHANNELS as u64);
        let outcome = reply_as(bystander, guess, "forged")?;
        check!(
            outcome == Err(ChannelError::NoTransaction)
                || outcome == Err(ChannelError::NotReceiver),
            "a guessed id was accepted: {outcome:?}"
        );
    }
    check!(
        channels::stats().drops == drops && channels::stats().replies == 0,
        "forgeries changed the counters: {:?}",
        channels::stats()
    );
    check!(
        channels::await_reply_now(txn) == Err(ChannelError::WouldBlock),
        "a refused forgery ended the call"
    );
    // A holder of the callee side that did not receive may answer (worker).
    check!(
        reply_as(worker, txn, "worker")? == Ok(()),
        "a callee-side holder was refused"
    );
    check!(
        payload(&channels::await_reply(txn).map_err(reason)?)? == "worker",
        "the caller did not get the worker's reply"
    );
    // The receiver answers a second call; a second answer is refused.
    let txn = channels::begin_call(client, 7, &request, None).map_err(reason)?;
    channels::recv(server, None).map_err(reason)?;
    channels::reply(txn, &parcel(8, 0, "mine")?).map_err(reason)?;
    check!(
        channels::reply(txn, &parcel(8, 0, "twice")?) == Err(ChannelError::NoTransaction),
        "a second reply was accepted"
    );
    check!(
        payload(&channels::await_reply(txn).map_err(reason)?)? == "mine",
        "wrong reply"
    );
    fresh()
}

/// A task that received a request and died before answering ends the call
/// with `PeerDied` while the side stays open for other holders; a request
/// nobody received stays open, and the dead slot's reuse cannot answer it.
pub fn receiver_death_ends_its_calls() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let request = parcel(7, flags::SYNC, "x")?;
    let served_by = spawn_holder("receiver", server, rights::ALL)?;
    let first = channels::begin_call(client, 7, &request, None).map_err(reason)?;
    task::harness::switch_current(served_by);
    channels::recv(0, None).map_err(reason)?;
    task::harness::switch_current(task::KERNEL_TASK);
    let queued_clients = shared_clients(client, 1)?;
    task::harness::switch_current(queued_clients[0].0);
    let second = channels::begin_call(queued_clients[0].1, 7, &request, None).map_err(reason)?;
    task::harness::switch_current(task::KERNEL_TASK);
    // The kernel task still holds `server`, so the side outlives the death.
    channels::close_endpoint_for(served_by, 0, true).map_err(reason)?;
    channels::forget_task(served_by);
    check!(
        channels::await_reply(first) == Err(ChannelError::PeerDied),
        "the dead receiver's call was not ended"
    );
    check!(
        channels::stats().outstanding == 1,
        "outstanding after the death: {:?}",
        channels::stats()
    );
    // The unreceived call is still answerable by the surviving holder.
    let message = channels::recv(server, None).map_err(reason)?;
    check!(message.txn == Some(second), "the queued call was lost");
    channels::reply(second, &parcel(8, 0, "ok")?).map_err(reason)?;
    task::harness::switch_current(queued_clients[0].0);
    check!(
        payload(&channels::await_reply(second).map_err(reason)?)? == "ok",
        "the surviving call got the wrong reply"
    );
    task::harness::switch_current(task::KERNEL_TASK);
    fresh()
}

/// Many calls in flight while every client sprays forged replies at every
/// transaction (its own, its neighbours', and guesses): none lands, and the
/// real replies, sent in reverse order, all reach their own callers.
pub fn reply_forgery_soak() -> Result<(), String> {
    const CLIENTS: usize = 8;
    const ROUNDS: usize = 1000;
    fresh()?;
    let (shared, server) = channels::create().map_err(reason)?;
    let clients = shared_clients(shared, CLIENTS)?;
    let mut refused = 0u64;
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
        let mut inbound = Vec::with_capacity(CLIENTS);
        for _ in 0..CLIENTS {
            inbound.push(channels::recv(server, None).map_err(reason)?);
        }
        let forged = parcel(8, 0, "forged")?;
        for &(slot, _) in &clients {
            task::harness::switch_current(slot);
            for &txn in &txns {
                for delta in 0..3u64 {
                    let guess = txn.wrapping_add(delta * channels::MAX_CHANNELS as u64);
                    match channels::reply(guess, &forged) {
                        Err(ChannelError::NotReceiver | ChannelError::NoTransaction) => {
                            refused += 1
                        }
                        other => {
                            return Err(format!("round {round}: forged reply gave {other:?}"));
                        }
                    }
                }
            }
        }
        task::harness::switch_current(task::KERNEL_TASK);
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
                "round {round}: client {slot} got a forged or wrong reply"
            );
        }
    }
    task::harness::switch_current(task::KERNEL_TASK);
    crate::trace::test!("ipc_channel_reply_forgery_soak:INFO:refused={refused}");
    let stats = channels::stats();
    let expected = (CLIENTS * ROUNDS) as u64;
    check!(
        stats.calls == expected
            && stats.replies == expected
            && stats.drops == 0
            && stats.outstanding == 0,
        "counters after the soak: {stats:?}"
    );
    fresh()
}

/// The receiver alone is enough: a task that took a request and then let go
/// of its handle (another holder keeps the side open) still answers it, and
/// once it is gone its reused slot cannot.
pub fn receiver_without_a_handle_replies() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let request = parcel(7, flags::SYNC, "x")?;
    let receiver = spawn_holder("receiver", server, rights::ALL)?;
    let txn = channels::begin_call(client, 7, &request, None).map_err(reason)?;
    task::harness::switch_current(receiver);
    channels::recv(0, None).map_err(reason)?;
    channels::release_endpoint(0).map_err(reason)?;
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        reply_as(receiver, txn, "late")? == Ok(()),
        "the receiver was refused after releasing its handle"
    );
    check!(
        payload(&channels::await_reply(txn).map_err(reason)?)? == "late",
        "wrong reply"
    );
    // A second call received by a task that then dies: the slot's next owner
    // (a fresh task with no handle) is a stranger to it.
    let txn = channels::begin_call(client, 7, &request, None).map_err(reason)?;
    let doomed = spawn_holder("doomed", server, rights::ALL)?;
    task::harness::switch_current(doomed);
    channels::recv(0, None).map_err(reason)?;
    task::harness::switch_current(task::KERNEL_TASK);
    channels::forget_task(doomed);
    handles::reset_for_task(doomed);
    check!(
        reply_as(doomed, txn, "inherited")? == Err(ChannelError::NotReceiver)
            || reply_as(doomed, txn, "inherited")? == Err(ChannelError::NoTransaction),
        "a reused slot answered its predecessor's call"
    );
    fresh()
}
