//! A message the receiver cannot take (audit H2, M-K1): too long for its
//! buffer, or with objects its table has no room for. The receiver survives,
//! the caller is told, and everything the message held is released.

use super::*;
use crate::ipc::handles::{rights, HandleKind, MAX_HANDLES};
use crate::ipc::shared;
use crate::tests::ipc_shared_suite::{
    channel_to, movable_end, parcel_with_objects, reap, spawn_receiver,
};
use alloc::vec;
use libmessenger::Object;

pub(super) fn buffer_reason(error: shared::Error) -> String {
    error.message().into()
}

/// The receive buffer services use (`DEFAULT_BUFFER`).
const BUFFER: usize = 16 * 1024;

fn big_text() -> String {
    "x".repeat(BUFFER + 512)
}

/// Nothing queued, outstanding or kept for a caller.
pub(super) fn tables_empty(what: &str) -> Result<(), String> {
    let stats = channels::stats();
    check!(
        stats.queued == 0 && stats.queued_bytes == 0 && stats.outstanding == 0,
        "{what}: left queued or outstanding: {stats:?}"
    );
    check!(
        channels::harness::live_transactions() == 0,
        "{what}: {} transactions left",
        channels::harness::live_transactions()
    );
    Ok(())
}

/// A request longer than the receive buffer is taken off the queue and
/// dropped: the receiver gets `Dropped` (not a dead endpoint), the caller
/// gets `PeerDied` at once, and the same endpoint serves the next request.
pub fn oversized_request_dropped() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let big = parcel(7, flags::SYNC, &big_text())?;
    let small = parcel(7, flags::SYNC, "fits")?;
    let answer = parcel(8, 0, "ok")?;

    let doomed = channels::begin_call(client, 7, &big, None).map_err(reason)?;
    check!(
        channels::recv_capped(server, None, BUFFER) == Err(ChannelError::Dropped),
        "an oversized request was not dropped"
    );
    check!(
        channels::await_reply(doomed) == Err(ChannelError::PeerDied),
        "the caller of a dropped request was left waiting"
    );
    tables_empty("after the drop")?;

    let txn = channels::begin_call(client, 7, &small, None).map_err(reason)?;
    let message = channels::recv_capped(server, None, BUFFER).map_err(reason)?;
    check!(message.txn == Some(txn), "the next request is not served");
    channels::reply(txn, &answer).map_err(reason)?;
    check!(
        channels::await_reply(txn).map_err(reason)? == answer,
        "the endpoint stopped answering after a drop"
    );
    tables_empty("after the next request")?;
    fresh()
}

/// The shapes of a drop: one-way oversize, a call whose objects the receiver
/// has no room for, and an oversized message that carried a channel end and a
/// buffer. Everything the message held is released and the moved end's peer
/// learns.
pub fn dropped_message_releases_objects() -> Result<(), String> {
    fresh()?;
    let creator = task::current();
    let child = spawn_receiver()?;
    let (client, child_server) = channel_to(child)?;
    let buffer = shared::create(4096).map_err(buffer_reason)?;
    let (end, kept) = movable_end()?;

    let big = parcel_with_objects(
        1,
        &big_text(),
        vec![Object::Channel(end), Object::Buffer(buffer)],
    )?;
    channels::send(client, &big).map_err(reason)?;
    task::harness::switch_current(child);
    check!(
        channels::recv_capped(child_server, None, BUFFER) == Err(ChannelError::Dropped),
        "an oversized one-way message was delivered"
    );
    check!(
        handles::count_for_task(child) == 1,
        "the drop installed {} handles",
        handles::count_for_task(child)
    );
    task::harness::switch_current(creator);
    check!(
        shared::info(buffer).map_err(buffer_reason)?.refs == 1,
        "the dropped message kept its buffer reference"
    );
    check!(
        channels::try_recv(kept) == Err(ChannelError::PeerDied),
        "the moved end's peer was not told"
    );

    // A call into a full handle table: dropped, and its caller told.
    let (end, kept) = movable_end()?;
    task::harness::switch_current(child);
    while handles::count_for_task(child) < MAX_HANDLES {
        handles::open(HandleKind::Object, rights::CALL, 0x11).map_err(|error| error.message())?;
    }
    task::harness::switch_current(creator);
    let call = parcel_with_objects(1, "call", vec![Object::Channel(end)])?;
    let txn = channels::begin_call(client, 1, &call, None).map_err(reason)?;
    task::harness::switch_current(child);
    check!(
        channels::recv_capped(child_server, None, BUFFER) == Err(ChannelError::Dropped),
        "a call into a full table was delivered"
    );
    handles::reset_for_task(child);
    task::harness::switch_current(creator);
    check!(
        channels::await_reply(txn) == Err(ChannelError::PeerDied),
        "the caller of an undeliverable call waits forever (M-K1)"
    );
    check!(
        channels::try_recv(kept) == Err(ChannelError::PeerDied),
        "the end of an undeliverable call was not closed"
    );
    shared::close(buffer).map_err(buffer_reason)?;
    tables_empty("after the drops")?;
    channels::reset();
    shared::reset();
    reap(child)
}

/// 3000 oversized calls and notes in a row: the receiver survives every one,
/// the callers all fail, and nothing stays queued or outstanding.
pub fn oversized_drop_soak() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let big = parcel(7, flags::SYNC, &big_text())?;
    let note = parcel(9, flags::ONE_WAY, &big_text())?;
    let small = parcel(7, flags::SYNC, "fits")?;
    let answer = parcel(8, 0, "ok")?;
    for round in 0..3000u32 {
        let txn = channels::begin_call(client, 7, &big, None).map_err(reason)?;
        channels::send(client, &note).map_err(reason)?;
        for _ in 0..2 {
            check!(
                channels::recv_capped(server, None, BUFFER) == Err(ChannelError::Dropped),
                "round {round}: an oversized message was delivered"
            );
        }
        check!(
            channels::await_reply(txn) == Err(ChannelError::PeerDied),
            "round {round}: the caller was not failed"
        );
        let txn = channels::begin_call(client, 7, &small, None).map_err(reason)?;
        let message = channels::recv_capped(server, None, BUFFER).map_err(reason)?;
        check!(message.txn == Some(txn), "round {round}: wrong request");
        channels::reply(txn, &answer).map_err(reason)?;
        channels::await_reply(txn).map_err(reason)?;
    }
    tables_empty("after the soak")?;
    channels::close_endpoint(client).map_err(reason)?;
    channels::close_endpoint(server).map_err(reason)?;
    check!(channels::counts().channels == 0, "a channel is left");
    fresh()
}

/// A poll (`POLL_DEADLINE`) whose request is too big is dropped like any
/// call: its caller is failed, nothing stays served or queued, and the next
/// poll is answered.
pub fn oversized_poll_dropped() -> Result<(), String> {
    fresh()?;
    let (client, server) = channels::create().map_err(reason)?;
    let big = parcel(7, flags::SYNC, &big_text())?;
    let small = parcel(7, flags::SYNC, "fits")?;
    let answer = parcel(8, 0, "ok")?;
    for round in 0..200u32 {
        let poll =
            channels::begin_call(client, 7, &big, Some(channels::POLL_DEADLINE)).map_err(reason)?;
        check!(
            channels::recv_capped(server, None, BUFFER) == Err(ChannelError::Dropped),
            "round {round}: an oversized poll was delivered"
        );
        check!(
            channels::await_reply(poll) == Err(ChannelError::PeerDied),
            "round {round}: the caller of a dropped poll was left waiting"
        );
        let txn = channels::begin_call(client, 7, &small, Some(channels::POLL_DEADLINE))
            .map_err(reason)?;
        let message = channels::recv_capped(server, None, BUFFER).map_err(reason)?;
        check!(message.txn == Some(txn), "round {round}: wrong poll");
        channels::reply(txn, &answer).map_err(reason)?;
        check!(
            channels::await_reply(txn).map_err(reason)? == answer,
            "round {round}: the next poll was not answered"
        );
    }
    tables_empty("after the polls")?;
    fresh()
}

/// `undo_delivery` (the copy-out of a received message failed after the
/// objects were installed): the installed handles close, the moved end's
/// peer is told, the buffer reference goes, and the request's caller is
/// failed. The syscall's own pre-check makes the real failure unreachable
/// from a test, so the undo is driven directly. 200 rounds.
pub fn undo_delivery_releases_everything() -> Result<(), String> {
    fresh()?;
    let buffer = shared::create(4096).map_err(buffer_reason)?;
    let held = handles::count_for_task(task::current());
    for round in 0..200u32 {
        let (client, server) = channels::create().map_err(reason)?;
        let (end, kept) = movable_end()?;
        let call = parcel_with_objects(
            1,
            "call",
            vec![Object::Channel(end), Object::Buffer(buffer)],
        )?;
        let txn = channels::begin_call(client, 1, &call, None).map_err(reason)?;
        let message = channels::recv_capped(server, None, BUFFER).map_err(reason)?;
        check!(
            message.objects.len() == 2,
            "round {round}: objects {:?}",
            message.objects
        );
        channels::undo_delivery(message);
        check!(
            channels::await_reply(txn) == Err(ChannelError::PeerDied),
            "round {round}: the caller of an undone delivery waits"
        );
        check!(
            channels::try_recv(kept) == Err(ChannelError::PeerDied),
            "round {round}: the moved end's peer was not told"
        );
        for handle in [client, server, kept] {
            channels::close_endpoint(handle).map_err(reason)?;
        }
    }
    check!(
        shared::info(buffer).map_err(buffer_reason)?.refs == 1,
        "the undone deliveries kept buffer references"
    );
    check!(
        handles::count_for_task(task::current()) == held,
        "{} handles leaked",
        handles::count_for_task(task::current()) - held
    );
    shared::close(buffer).map_err(buffer_reason)?;
    tables_empty("after the undone deliveries")?;
    fresh()
}
