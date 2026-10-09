//! Channel moves and buffer shares across tasks, the kind of each object
//! slot, and a zero-copy handoff.

use super::*;

/// A message moves a channel end and shares a buffer across two tasks: the
/// moved end's sender number is gone, the shared buffer's stays, the
/// receiver's table gets fresh numbers with the same rights, in list order,
/// and a handle without `TRANSFER` is refused.
pub fn buffer_handle_transfer_rights() -> Result<(), String> {
    fresh()?;
    let creator = task::current();
    let child = spawn_receiver()?;
    let (client, child_server) = channel_to(child)?;

    let (movable, kept) = movable_end()?;
    let movable_entry = handles::get(movable).map_err(handle_reason)?;
    let stuck = handles::duplicate(movable, rights::CALL).map_err(handle_reason)?;
    let buffer = shared::create(4096).map_err(buffer_reason)?;

    // A handle without TRANSFER is refused and nothing moves.
    let refused = parcel_with_objects(1, "no", vec![Object::Channel(stuck)])?;
    check!(
        channels::send(client, &refused) == Err(ChannelError::MissingRight),
        "a handle without TRANSFER rights was transferred"
    );
    check!(
        handles::get(stuck).is_ok(),
        "the refused transfer moved the sender's handle"
    );

    let bytes = parcel_with_objects(
        1,
        "yes",
        vec![Object::Channel(movable), Object::Buffer(buffer)],
    )?;
    channels::send(client, &bytes).map_err(channel_reason)?;
    check!(
        handles::get(movable) == Err(HandleError::InvalidHandle),
        "the move did not close the sender's channel handle"
    );
    check!(
        handles::get(buffer).is_ok(),
        "sharing a buffer took the sender's handle"
    );
    check!(
        shared::info(buffer).map_err(buffer_reason)?.refs == 2,
        "the queued message holds no reference of its own"
    );

    task::harness::switch_current(child);
    let message = channels::try_recv(child_server)
        .map_err(channel_reason)?
        .ok_or("the transferred message is missing")?;
    check!(
        message.objects.len() == 2,
        "delivered {} objects, expected 2",
        message.objects.len()
    );
    let (end_handle, buffer_handle) = (message.objects[0], message.objects[1]);
    let end_entry = handles::get(end_handle).map_err(handle_reason)?;
    check!(
        end_entry.kind == HandleKind::Channel
            && end_entry.object_id == movable_entry.object_id
            && end_entry.rights == movable_entry.rights,
        "the received channel handle is {end_entry:?}, sent {movable_entry:?}"
    );
    let buffer_entry = handles::get(buffer_handle).map_err(handle_reason)?;
    check!(
        buffer_entry.kind == HandleKind::Buffer,
        "the received buffer handle is {buffer_entry:?}"
    );
    // The receiver owns a mapping of the very same frames.
    let receiver_va = shared::map(buffer_handle).map_err(buffer_reason)?;
    check!(
        raw_entry(mem::kernel_table(), receiver_va).is_some(),
        "the receiver's mapping is missing"
    );

    shared::close(buffer_handle).map_err(buffer_reason)?;
    channels::close_endpoint(end_handle).map_err(channel_reason)?;
    handles::reset_for_task(child);
    task::harness::switch_current(creator);
    check!(
        shared::info(buffer).map_err(buffer_reason)?.refs == 1,
        "the receiver's close left its reference"
    );
    // The receiver closed the moved end: its peer in the creator sees it.
    check!(
        channels::try_recv(kept) == Err(ChannelError::PeerDied),
        "the moved end's peer did not observe the close"
    );
    shared::close(buffer).map_err(buffer_reason)?;
    handles::close(stuck).ok();
    channels::reset();
    shared::reset();
    reap(child)?;
    Ok(())
}

/// An object-list entry must name a handle of its slot's kind: a buffer in a
/// channel slot, or a channel end in a buffer slot, is refused
/// (`WrongObjectKind`, `EINVAL` at the gate) before anything moves: the
/// sender's table is as it was, the buffer's reference count too, and
/// nothing reaches the receiver.
pub fn object_kind_mismatch_refused() -> Result<(), String> {
    fresh()?;
    let creator = task::current();
    let child = spawn_receiver()?;
    let (client, child_server) = channel_to(child)?;
    let buffer = shared::create(4096).map_err(buffer_reason)?;
    let (movable, _kept) = movable_end()?;
    let before = handles::count();

    let cases = [
        vec![Object::Channel(buffer), Object::Channel(movable)],
        vec![Object::Channel(movable), Object::Channel(buffer)],
        vec![Object::Buffer(movable)],
        vec![Object::Buffer(buffer), Object::Buffer(movable)],
    ];
    for objects in cases {
        let bytes = parcel_with_objects(1, "moved?", objects.clone())?;
        check!(
            channels::send(client, &bytes) == Err(ChannelError::WrongObjectKind),
            "a mismatched object list was accepted: {objects:?}"
        );
    }
    check!(
        handles::get(buffer).is_ok() && handles::get(movable).is_ok(),
        "the refused send changed the sender's table"
    );
    check!(
        handles::count() == before,
        "the sender's table has {} handles, had {before}",
        handles::count()
    );
    let info = shared::info(buffer).map_err(buffer_reason)?;
    check!(
        info.refs == 1 && info.mappings == 1,
        "the refused send touched the buffer: {info:?}"
    );
    check!(
        channels::stats().queued == 0,
        "the refused send queued a message"
    );

    task::harness::switch_current(child);
    check!(
        channels::try_recv(child_server)
            .map_err(channel_reason)?
            .is_none(),
        "the receiver got a message from a refused send"
    );
    handles::reset_for_task(child);
    task::harness::switch_current(creator);
    shared::close(buffer).map_err(buffer_reason)?;
    channels::reset();
    shared::reset();
    reap(child)?;
    Ok(())
}

/// A buffer handoff moves no data: the receiver's mapping resolves to the
/// very frames the creator wrote, and the handoff counter advances.
pub fn buffer_zero_copy_handoff() -> Result<(), String> {
    fresh()?;
    let creator = task::current();
    let child = spawn_receiver()?;
    let (client, child_server) = channel_to(child)?;

    let size = 2 * 4096;
    let handle = shared::create(size).map_err(buffer_reason)?;
    let creator_va = shared::map(handle).map_err(buffer_reason)?;
    let mut creator_frames = Vec::new();
    for page in 0..2u64 {
        creator_frames.push(
            frame_of(mem::kernel_table(), creator_va + page * 4096)
                .map_err(|error| format!("creator page {page}: {error}"))?,
        );
        for offset in 0..4096usize {
            let at = creator_va + page * 4096 + offset as u64;
            // Safety: the buffer is mapped read/write.
            unsafe { (at as *mut u8).write_volatile(pattern_byte(page as u8, offset)) };
        }
    }

    // The message shares the buffer; the creator then drops its own handle
    // and mapping, so the message's reference is the only one left.
    let bytes = parcel_with_objects(5, "surface", vec![Object::Buffer(handle)])?;
    channels::send(client, &bytes).map_err(channel_reason)?;
    shared::close(handle).map_err(buffer_reason)?;

    task::harness::switch_current(child);
    let message = channels::try_recv(child_server)
        .map_err(channel_reason)?
        .ok_or("the transferred message is missing")?;
    check!(
        message.objects.len() == 1,
        "delivered {} objects, expected 1",
        message.objects.len()
    );
    let received = message.objects[0];
    let receiver_va = shared::map(received).map_err(buffer_reason)?;
    // The creator's mapping is gone, so its virtual range may be recycled
    // for the receiver; what matters is the frames.
    for (page, expected) in creator_frames.iter().enumerate() {
        let actual = frame_of(mem::kernel_table(), receiver_va + page as u64 * 4096)
            .map_err(|error| format!("receiver page {page}: {error}"))?;
        check!(
            actual == *expected,
            "page {page} was copied: creator {expected:#x}, receiver {actual:#x}"
        );
        for offset in (0..4096usize).step_by(53) {
            let at = receiver_va + page as u64 * 4096 + offset as u64;
            // Safety: the receiver's mapping is readable.
            let got = unsafe { (at as *const u8).read_volatile() };
            check!(
                got == pattern_byte(page as u8, offset),
                "receiver read {got:#x} at page {page} offset {offset}"
            );
        }
    }
    let stats = shared::stats();
    check!(
        stats.handoffs == 1,
        "zero-copy handoffs counted {}, expected 1",
        stats.handoffs
    );
    serial_println!(
        "TEST:ipc_buffer_zero_copy_handoff:INFO:frames={} bytes={size} copies=0",
        creator_frames.len()
    );

    shared::close(received).map_err(buffer_reason)?;
    handles::reset_for_task(child);
    task::harness::switch_current(creator);
    channels::reset();
    shared::reset();
    reap(child)?;
    Ok(())
}
