//! The object rules of `docs/messenger-core-plan.md` 2.3 beyond the kind
//! gate: one move per channel end, no objects in a reply, a delivery that
//! cannot install everything rolls back, and a long move/share soak.

use super::*;
use crate::ipc::handles::MAX_HANDLES;

/// A channel end appears at most once in a message: a second entry for the
/// same handle is refused (`BadTransfer`) and the end stays with the sender.
pub fn object_duplicate_channel_refused() -> Result<(), String> {
    fresh()?;
    let creator = task::current();
    let child = spawn_receiver()?;
    let (client, child_server) = channel_to(child)?;
    let (movable, _kept) = movable_end()?;
    let buffer = shared::create(4096).map_err(buffer_reason)?;

    let twice = parcel_with_objects(
        1,
        "twice",
        vec![Object::Channel(movable), Object::Channel(movable)],
    )?;
    check!(
        channels::send(client, &twice) == Err(ChannelError::BadTransfer),
        "a channel end listed twice was accepted"
    );
    check!(
        handles::get(movable).is_ok(),
        "the refused send moved the channel end"
    );
    // A buffer may be shared twice in one message: each entry is a reference.
    let shared_twice = parcel_with_objects(
        1,
        "two refs",
        vec![Object::Buffer(buffer), Object::Buffer(buffer)],
    )?;
    channels::send(client, &shared_twice).map_err(channel_reason)?;
    check!(
        shared::info(buffer).map_err(buffer_reason)?.refs == 3,
        "two shares did not take two references"
    );

    task::harness::switch_current(child);
    let message = channels::try_recv(child_server)
        .map_err(channel_reason)?
        .ok_or("the shared message is missing")?;
    check!(
        message.objects.len() == 2,
        "delivered {:?}",
        message.objects
    );
    for handle in &message.objects {
        shared::close(*handle).map_err(buffer_reason)?;
    }
    handles::reset_for_task(child);
    task::harness::switch_current(creator);
    check!(
        shared::info(buffer).map_err(buffer_reason)?.refs == 1,
        "the receiver's closes left a reference"
    );
    shared::close(buffer).map_err(buffer_reason)?;
    channels::reset();
    shared::reset();
    reap(child)?;
    Ok(())
}

/// A reply carries no objects: one with any is refused
/// (`UnsupportedTransfer`), the transaction stays open for a plain reply,
/// and the sender keeps what it tried to send.
pub fn object_reply_refused() -> Result<(), String> {
    fresh()?;
    let creator = task::current();
    let child = spawn_receiver()?;
    let (client, child_server) = channel_to(child)?;
    let request = parcel_with_objects(3, "call", Vec::new())?;
    let txn = channels::begin_call(client, 3, &request, None).map_err(channel_reason)?;

    task::harness::switch_current(child);
    let message = channels::try_recv(child_server)
        .map_err(channel_reason)?
        .ok_or("the call never arrived")?;
    check!(
        message.txn == Some(txn),
        "the call's transaction is {:?}",
        message.txn
    );
    let (end, _kept) = movable_end()?;
    let buffer = shared::create(4096).map_err(buffer_reason)?;
    for objects in [vec![Object::Channel(end)], vec![Object::Buffer(buffer)]] {
        let reply = parcel_with_objects(3, "answer", objects.clone())?;
        check!(
            channels::reply(txn, &reply) == Err(ChannelError::UnsupportedTransfer),
            "a reply carrying {objects:?} was accepted"
        );
    }
    check!(
        handles::get(end).is_ok() && shared::info(buffer).map_err(buffer_reason)?.refs == 1,
        "a refused reply moved or shared its objects"
    );
    let plain = parcel_with_objects(3, "answer", Vec::new())?;
    channels::reply(txn, &plain).map_err(channel_reason)?;
    shared::close(buffer).map_err(buffer_reason)?;
    handles::reset_for_task(child);
    task::harness::switch_current(creator);
    let answer = channels::await_reply(txn).map_err(channel_reason)?;
    check!(answer == plain, "the plain reply did not arrive intact");
    channels::reset();
    shared::reset();
    reap(child)?;
    Ok(())
}

/// Fill `slot`'s table until `free` handles remain.
fn fill_table(slot: usize, free: usize) -> Result<(), String> {
    let caller = task::current();
    task::harness::switch_current(slot);
    let mut outcome = Ok(());
    while handles::count_for_task(slot) + free < MAX_HANDLES {
        if let Err(error) = handles::open(HandleKind::Object, rights::CALL, 0x11) {
            outcome = Err(format!("filling the table: {}", error.message()));
            break;
        }
    }
    task::harness::switch_current(caller);
    outcome
}

/// A delivery the receiver's table cannot hold is rolled back whole: the
/// objects installed so far are closed again, the buffer references the
/// message held are released, and a channel end nobody received is closed
/// so its peer learns.
pub fn object_rollback_on_full_table() -> Result<(), String> {
    fresh()?;
    let creator = task::current();
    let child = spawn_receiver()?;
    let (client, child_server) = channel_to(child)?;
    let buffer = shared::create(4096).map_err(buffer_reason)?;

    // One free slot: the channel end installs, the buffer cannot.
    fill_table(child, 1)?;
    let (end, kept) = movable_end()?;
    let bytes = parcel_with_objects(1, "two", vec![Object::Channel(end), Object::Buffer(buffer)])?;
    channels::send(client, &bytes).map_err(channel_reason)?;
    check!(
        shared::info(buffer).map_err(buffer_reason)?.refs == 2,
        "the queued message took no reference"
    );
    let held = handles::count_for_task(child);
    task::harness::switch_current(child);
    let outcome = channels::try_recv(child_server);
    check!(
        outcome == Err(ChannelError::NoFreeHandle),
        "a delivery into a full table did not fail: {outcome:?}"
    );
    check!(
        handles::count_for_task(child) == held,
        "the rolled-back delivery left a handle: {} of {held}",
        handles::count_for_task(child)
    );
    task::harness::switch_current(creator);
    check!(
        shared::info(buffer).map_err(buffer_reason)?.refs == 1,
        "the rolled-back delivery kept the message's buffer reference"
    );
    check!(
        channels::try_recv(kept) == Err(ChannelError::PeerDied),
        "the orphaned channel end was not closed"
    );

    // No free slot at all: the first object already fails.
    fill_table(child, 0)?;
    let bytes = parcel_with_objects(1, "one", vec![Object::Buffer(buffer)])?;
    channels::send(client, &bytes).map_err(channel_reason)?;
    task::harness::switch_current(child);
    check!(
        channels::try_recv(child_server) == Err(ChannelError::NoFreeHandle),
        "a delivery into a full table did not fail"
    );
    handles::reset_for_task(child);
    task::harness::switch_current(creator);
    check!(
        shared::info(buffer).map_err(buffer_reason)?.refs == 1,
        "the second rollback kept a reference"
    );
    shared::close(buffer).map_err(buffer_reason)?;
    channels::reset();
    shared::reset();
    reap(child)?;
    Ok(())
}

/// Move a channel end from the current task to `to` over `channel`, receive
/// it there, and return the number it landed under; the caller is `from`.
fn move_end(from: usize, to: usize, channel: u64, server: u64, end: u64) -> Result<u64, String> {
    let bytes = parcel_with_objects(1, "move", vec![Object::Channel(end)])?;
    channels::send(channel, &bytes).map_err(channel_reason)?;
    task::harness::switch_current(to);
    let message = channels::try_recv(server)
        .map_err(channel_reason)?
        .ok_or("the moved end never arrived")?;
    let landed = *message
        .objects
        .first()
        .ok_or("the move delivered nothing")?;
    task::harness::switch_current(from);
    Ok(landed)
}

/// Move one channel end back and forth between two tasks and share one
/// buffer with the receiver, many times over: handle counts, buffer counts
/// and free frames end exactly where they started, and the end is still the
/// same object with its peer still open.
pub fn object_move_share_soak() -> Result<(), String> {
    const ROUNDS: usize = 100_000;
    const WARMUP: usize = 2_000;
    fresh()?;
    let creator = task::current();
    let child = spawn_receiver()?;
    // Creator -> child, and child -> creator (the mirror trick in reverse:
    // the child sends on a handle of its own to a side the creator holds).
    let (to_child, child_server) = channel_to(child)?;
    let (back_client, back_server) = channels::create().map_err(channel_reason)?;
    let child_sender = {
        let entry = handles::get(back_client).map_err(handle_reason)?;
        task::harness::switch_current(child);
        let handle = handles::open(HandleKind::Channel, entry.rights, entry.object_id)
            .map_err(handle_reason)?;
        task::harness::switch_current(creator);
        handles::close(back_client).map_err(handle_reason)?;
        handle
    };
    let (mut end, kept) = movable_end()?;
    let identity = handles::get(end).map_err(handle_reason)?.object_id;
    let buffer = shared::create(4096).map_err(buffer_reason)?;

    let mut creator_held = 0;
    let mut child_held = 0;
    let mut buffers = 0;
    let mut frames = 0;
    for round in 0..WARMUP + ROUNDS {
        if round == WARMUP {
            creator_held = handles::count_for_task(creator);
            child_held = handles::count_for_task(child);
            buffers = shared::stats().buffers;
            frames = mem::frame_stats().free;
        }
        let there = move_end(creator, child, to_child, child_server, end)?;
        task::harness::switch_current(child);
        let back = move_end(child, creator, child_sender, back_server, there)?;
        task::harness::switch_current(creator);
        end = back;
        let share = parcel_with_objects(2, "share", vec![Object::Buffer(buffer)])?;
        channels::send(to_child, &share).map_err(channel_reason)?;
        task::harness::switch_current(child);
        let message = channels::try_recv(child_server)
            .map_err(channel_reason)?
            .ok_or("the shared buffer never arrived")?;
        shared::close(message.objects[0]).map_err(buffer_reason)?;
        task::harness::switch_current(creator);
    }
    check!(
        handles::get(end).map_err(handle_reason)?.object_id == identity,
        "the end that came back names another object"
    );
    check!(
        handles::count_for_task(creator) == creator_held
            && handles::count_for_task(child) == child_held,
        "handles drifted: creator {} (was {creator_held}), child {} (was {child_held})",
        handles::count_for_task(creator),
        handles::count_for_task(child)
    );
    check!(
        shared::stats().buffers == buffers
            && shared::info(buffer).map_err(buffer_reason)?.refs == 1,
        "buffers drifted: {} live (was {buffers}), refs {}",
        shared::stats().buffers,
        shared::info(buffer).map_err(buffer_reason)?.refs
    );
    check!(
        mem::frame_stats().free == frames,
        "frames drifted: {frames} free before, {} after",
        mem::frame_stats().free
    );
    // The end's peer never noticed a thing.
    check!(
        channels::try_recv(kept) == Ok(None),
        "the moved end's peer saw a close"
    );
    serial_println!(
        "TEST:ipc_object_move_share_soak:INFO:moves={} shares={ROUNDS}",
        2 * ROUNDS
    );
    shared::close(buffer).map_err(buffer_reason)?;
    handles::reset_for_task(child);
    channels::reset();
    shared::reset();
    reap(child)?;
    Ok(())
}
