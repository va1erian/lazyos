//! The indexed registry and the one-copy parcel path (docs/performance-plan.md
//! P6.3): stale ids never reach a reused slot, a million calls leave the
//! quota and the heap exactly where they were, and a caller that dies inside
//! a call is forgotten without a trace.

use core::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::quota::{self, Resource};
use crate::task::wait::WaitQueue;
use crate::task::PriorityClass;

/// The queue charges of the kernel task's user (uid 0).
fn queue_charges() -> (u64, u64) {
    (
        quota::usage(0, Resource::QueueBytes),
        quota::usage(0, Resource::QueueDepth),
    )
}

/// 5000 channels created, used and closed in turn: every one reuses a slot
/// of the fixed registry, yet its id is new, a transaction id of an earlier
/// channel in the same slot is refused, and the registry ends empty.
pub fn index_reuse_soak() -> Result<(), String> {
    fresh()?;
    let request = parcel(1, flags::SYNC, "ping")?;
    let answer = parcel(1, 0, "pong")?;
    let mut previous: Option<(u64, u64)> = None;
    for round in 0..5000u32 {
        let (client, server) = channels::create().map_err(reason)?;
        let id = handles::get(client)
            .map_err(|error| error.message())?
            .object_id;
        let txn = channels::begin_call(client, 1, &request, None).map_err(reason)?;
        if let Some((old_id, old_txn)) = previous {
            check!(id != old_id, "round {round}: channel id {id:#x} reused");
            check!(
                channels::reply(old_txn, &answer) == Err(ChannelError::NoTransaction),
                "round {round}: a stale transaction id reached the new channel"
            );
        }
        let message = channels::recv(server, None).map_err(reason)?;
        check!(message.txn == Some(txn), "round {round}: wrong transaction");
        channels::reply(txn, &answer).map_err(reason)?;
        check!(
            payload(&channels::await_reply(txn).map_err(reason)?)? == "pong",
            "round {round}: wrong reply"
        );
        channels::close_endpoint(client).map_err(reason)?;
        channels::close_endpoint(server).map_err(reason)?;
        previous = Some((id, txn));
    }
    check!(
        channels::counts().channels == 0,
        "{} channels left",
        channels::counts().channels
    );
    Ok(())
}

/// One million round trips, with a one-way message every fourth, through the
/// owned (one-copy) entry points: the uid's queue quota returns exactly to
/// where it was, nothing stays queued or outstanding, and the heap does not
/// grow.
pub fn million_calls_quota_exact() -> Result<(), String> {
    fresh()?;
    let request = parcel(1, flags::SYNC, "ping")?;
    let note = parcel(2, 0, "note")?;
    let answer = parcel(1, 0, "pong")?;
    let (client, server) = channels::create().map_err(reason)?;
    let charges = queue_charges();
    // Warm up so every vector reaches its steady capacity first.
    let mut heap_before = 0;
    for round in 0..1_000_000u32 {
        if round == 1000 {
            heap_before = mem::heap_stats().used;
        }
        if round % 4 == 0 {
            channels::send_owned(client, note.clone()).map_err(reason)?;
            let message = channels::recv(server, None).map_err(reason)?;
            check!(message.txn.is_none(), "round {round}: a note had a txn");
        }
        let txn = channels::begin_call_owned(client, 1, request.clone(), None).map_err(reason)?;
        let message = channels::recv(server, None).map_err(reason)?;
        check!(message.txn == Some(txn), "round {round}: wrong transaction");
        channels::reply_owned(txn, answer.clone()).map_err(reason)?;
        let reply = channels::await_reply(txn).map_err(reason)?;
        check!(
            reply == answer,
            "round {round}: the reply changed in transit"
        );
    }
    let heap_after = mem::heap_stats().used;
    let stats = channels::stats();
    check!(
        stats.queued == 0 && stats.queued_bytes == 0 && stats.outstanding == 0,
        "left behind: {stats:?}"
    );
    check!(
        queue_charges() == charges,
        "queue charges {:?} after, {charges:?} before",
        queue_charges()
    );
    check!(
        heap_after <= heap_before,
        "the heap grew from {heap_before} to {heap_after} bytes over the calls"
    );
    channels::close_endpoint(client).map_err(reason)?;
    channels::close_endpoint(server).map_err(reason)?;
    Ok(())
}

/// The handle the caller thread calls on, and where it parks afterwards.
static CALLER_HANDLE: AtomicU64 = AtomicU64::new(0);
static CALLER_QUEUE: WaitQueue = WaitQueue::new(WaitKind::Sleep);

/// A thread that makes one call and would park after it; the test ends it
/// while it waits for the reply.
extern "C" fn caller() -> ! {
    let me = task::current();
    if let Ok(request) = parcel(3, flags::SYNC, "never answered") {
        let _ = channels::call(CALLER_HANDLE.load(Ordering::Relaxed), 3, &request, None);
    }
    loop {
        CALLER_QUEUE.wait(me, None);
    }
}

/// 300 times: a real thread calls the kernel task, which receives the
/// request (the call hands the CPU straight to it), then ends the caller
/// while it is parked in `await_reply` and reaps it. The reply finds no
/// transaction, the caller's charges and meters are gone, no waiter is left
/// on the Messenger queue and every slot comes back.
pub fn caller_dies_mid_call_soak() -> Result<(), String> {
    fresh()?;
    let free = task::free_slots();
    let answer = parcel(3, 0, "too late")?;
    let charges = queue_charges();
    let waiters = channels::harness::queued_waiters();
    for round in 0..300u32 {
        let (client, server) = channels::create().map_err(reason)?;
        let entry = handles::get(client).map_err(|error| error.message())?;
        let slot = x86_64::instructions::interrupts::without_interrupts(|| {
            task::kthread::spawn_kernel_thread("caller", caller, PriorityClass::Normal)
        })
        .map_err(|error| format!("round {round}: spawn: {error}"))?;
        handles::reset_for_task(slot);
        let handle = handles::open_for_task(slot, entry.kind, entry.rights, entry.object_id)
            .map_err(|error| error.message())?;
        CALLER_HANDLE.store(handle, Ordering::Relaxed);
        // The kernel task parks; the thread runs, calls, and parks in turn.
        let message = channels::recv(server, None).map_err(reason)?;
        let txn = message.txn.ok_or("the request carried no transaction")?;
        check!(
            message.sender == slot && blocked_call(slot, None),
            "round {round}: sender {} state {:?}",
            message.sender,
            task::harness::state(slot)
        );
        task::harness::finish(slot, 9);
        let reaped = task::reap_child();
        check!(
            reaped.is_some_and(|(child, status)| child == slot && status == 9),
            "round {round}: reaped {reaped:?}"
        );
        check!(
            channels::reply(txn, &answer) == Err(ChannelError::NoTransaction),
            "round {round}: the dead caller's transaction survived its teardown"
        );
        let stats = channels::stats();
        check!(
            stats.outstanding == 0 && stats.queued == 0,
            "round {round}: left behind {stats:?}"
        );
        channels::close_endpoint(client).map_err(reason)?;
        channels::close_endpoint(server).map_err(reason)?;
    }
    check!(
        queue_charges() == charges,
        "queue charges {:?} after, {charges:?} before",
        queue_charges()
    );
    check!(
        channels::harness::queued_waiters() == waiters,
        "{} Messenger waiters after, {waiters} before",
        channels::harness::queued_waiters()
    );
    check!(
        task::free_slots() == free,
        "free slots {free} -> {}",
        task::free_slots()
    );
    task::harness::reset();
    Ok(())
}
