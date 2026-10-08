//! Handle moves and buffer shares across tasks, the one path for a buffer,
//! fence submit/wait, and a zero-copy handoff.

use super::*;

/// A message moves handles and shares buffers across two tasks: a moved
/// handle's sender number is gone, a shared buffer's stays, the receiver's
/// table gets fresh numbers with the same rights, and a handle without
/// `TRANSFER` is refused.
pub fn buffer_handle_transfer_rights() -> Result<(), String> {
    fresh()?;
    let creator = task::current();
    let child = spawn_receiver()?;
    let (client, child_server) = channel_to(child)?;

    let movable = handles::open(HandleKind::Object, rights::CALL | rights::TRANSFER, 0xabc)
        .map_err(handle_reason)?;
    let stuck = handles::open(HandleKind::Object, rights::CALL, 0xdef).map_err(handle_reason)?;
    let buffer =
        shared::create(4096, shared::flags::READ | shared::flags::WRITE).map_err(buffer_reason)?;

    // A handle without TRANSFER is refused and nothing moves.
    let refused = parcel_with_transfers(1, "no", vec![stuck], Vec::new())?;
    check!(
        channels::send(client, &refused) == Err(ChannelError::MissingRight),
        "a handle without TRANSFER rights was transferred"
    );
    check!(
        handles::get(stuck).is_ok(),
        "the refused transfer moved the sender's handle"
    );

    let bytes = parcel_with_transfers(1, "yes", vec![movable], vec![share(buffer)?])?;
    channels::send(client, &bytes).map_err(channel_reason)?;
    check!(
        handles::get(movable) == Err(HandleError::InvalidHandle),
        "the transfer did not move the sender's object handle"
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
        message.handles.len() == 1 && message.buffers.len() == 1,
        "delivered {} handles and {} buffers, expected 1 and 1",
        message.handles.len(),
        message.buffers.len()
    );
    let object_handle = message.handles[0];
    let buffer_handle = message.buffers[0].handle;
    let object_entry = handles::get(object_handle).map_err(handle_reason)?;
    check!(
        object_entry.kind == HandleKind::Object
            && object_entry.object_id == 0xabc
            && object_entry.rights == rights::CALL | rights::TRANSFER,
        "the received object handle is {object_entry:?}"
    );
    check!(
        handles::duplicate(object_handle, rights::CALL) == Err(HandleError::MissingRight),
        "the received handle did not obey its missing DUPLICATE right"
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
    handles::close(object_handle).ok();
    handles::reset_for_task(child);
    task::harness::switch_current(creator);
    check!(
        shared::info(buffer).map_err(buffer_reason)?.refs == 1,
        "the receiver's close left its reference"
    );
    shared::close(buffer).map_err(buffer_reason)?;
    handles::close(stuck).ok();
    channels::reset();
    shared::reset();
    reap(child)?;
    Ok(())
}

/// A buffer handle in the `handles` vector is refused (`BufferInHandles`,
/// `EINVAL` at the gate) before anything moves: the sender's table is as it
/// was, the buffer's reference count too, and nothing reaches the receiver.
pub fn buffer_in_handles_refused() -> Result<(), String> {
    fresh()?;
    let creator = task::current();
    let child = spawn_receiver()?;
    let (client, child_server) = channel_to(child)?;
    let buffer =
        shared::create(4096, shared::flags::READ | shared::flags::WRITE).map_err(buffer_reason)?;
    let movable = handles::open(HandleKind::Object, rights::CALL | rights::TRANSFER, 0xabc)
        .map_err(handle_reason)?;
    let before = handles::count();

    // The buffer first, then an object behind it: refused as a whole.
    let bytes = parcel_with_transfers(1, "moved?", vec![buffer, movable], Vec::new())?;
    check!(
        channels::send(client, &bytes) == Err(ChannelError::BufferInHandles),
        "a buffer handle in `handles` was accepted"
    );
    // The object first, then the buffer: the object must not have moved.
    let bytes = parcel_with_transfers(1, "moved?", vec![movable, buffer], Vec::new())?;
    check!(
        channels::send(client, &bytes) == Err(ChannelError::BufferInHandles),
        "a buffer handle behind an object was accepted"
    );
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
    handles::close(movable).ok();
    channels::reset();
    shared::reset();
    reap(child)?;
    Ok(())
}

/// A submitted fence resolves a wait, a park is woken by a later submit,
/// and a wait past its deadline reports `TimedOut`.
pub fn buffer_fence_submit_wait() -> Result<(), String> {
    fresh()?;
    let handle =
        shared::create(4096, shared::flags::READ | shared::flags::WRITE).map_err(buffer_reason)?;

    // Nothing submitted yet: an already-expired wait times out.
    check!(
        shared::fence_wait(handle, 1, Some(task::ticks())) == Err(BufferError::TimedOut),
        "fence_wait returned before its sequence was submitted"
    );
    check!(
        task::harness::state(task::current()) == Some(TaskState::Runnable),
        "the waiter stayed parked after the timeout"
    );

    // Park without yielding, then submit: the wake path resolves it.
    let parked = shared::harness::park_wait(handle, 7, None).map_err(buffer_reason)?;
    check!(!parked, "park_wait claimed the sequence was submitted");
    check!(
        matches!(
            task::harness::state(task::current()),
            Some(TaskState::Blocked { .. })
        ),
        "park_wait did not block the waiter"
    );
    shared::fence_submit(handle, 7).map_err(buffer_reason)?;
    check!(
        task::harness::state(task::current()) == Some(TaskState::Runnable),
        "fence_submit did not wake the parked waiter"
    );
    check!(
        task::harness::take_wake_reason(task::current()) == Some(WakeReason::Woken),
        "the fence wake reason is not Woken"
    );
    // The real wait resolves immediately once the sequence is there.
    shared::fence_wait(handle, 7, None).map_err(buffer_reason)?;

    // A deadline sweep wakes a parked waiter with TimedOut.
    let deadline = task::ticks() + 10;
    let parked = shared::harness::park_wait(handle, 9, Some(deadline)).map_err(buffer_reason)?;
    check!(!parked, "park_wait claimed the sequence was submitted");
    task::harness::expire_deadlines(deadline);
    check!(
        task::harness::state(task::current()) == Some(TaskState::Runnable),
        "the deadline sweep did not wake the fence waiter"
    );
    check!(
        task::harness::take_wake_reason(task::current()) == Some(WakeReason::TimedOut),
        "the deadline wake reason is not TimedOut"
    );

    // Sequences are monotonic and the meters track the waits.
    check!(
        shared::fence_submit(handle, 3) == Err(BufferError::StaleSequence),
        "a stale fence sequence was accepted"
    );
    let info = shared::info(handle).map_err(buffer_reason)?;
    check!(
        info.submitted == 7 && info.waited == 7,
        "fence state is {info:?}"
    );
    let stats = shared::stats();
    check!(
        stats.fence_waits == 1 && stats.fence_timeouts == 1,
        "fence stats are {stats:?}"
    );
    let process = shared::process_stats(task::current());
    check!(
        process.fence_waits == 1 && process.fence_timeouts == 1,
        "process fence stats are {process:?}"
    );
    shared::close(handle).map_err(buffer_reason)?;
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
    let handle =
        shared::create(size, shared::flags::READ | shared::flags::WRITE).map_err(buffer_reason)?;
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
    let bytes = parcel_with_transfers(5, "surface", Vec::new(), vec![share(handle)?])?;
    channels::send(client, &bytes).map_err(channel_reason)?;
    shared::close(handle).map_err(buffer_reason)?;

    task::harness::switch_current(child);
    let message = channels::try_recv(child_server)
        .map_err(channel_reason)?
        .ok_or("the transferred message is missing")?;
    check!(
        message.buffers.len() == 1,
        "delivered {} buffers, expected 1",
        message.buffers.len()
    );
    let received = message.buffers[0].handle;
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
