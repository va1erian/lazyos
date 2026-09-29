//! Receiving and delivering queued messages.

use super::*;

/// Receive the next message without blocking; `Ok(None)` means "try later".
///
/// Delivery installs the message's transferred handles and buffers into the
/// calling task's handle table and rewrites them to local numbers, so the
/// returned [`Message`] is immediately usable.
pub fn try_recv(handle: u64) -> Result<Option<Message>, Error> {
    let (channel_id, side) = endpoint_of(handle, rights::CALL)?;
    let queued = {
        let mut channels = CHANNELS.lock();
        let channel = find_channel(&mut channels, channel_id)?;
        let endpoint = &mut channel.endpoints[side];
        if let Some(message) = endpoint.inbox.pop_front() {
            endpoint.queued_bytes = endpoint.queued_bytes.saturating_sub(message.bytes.len());
            Some(message)
        } else if channel.endpoints[1 - side].closed {
            return Err(Error::PeerDied);
        } else {
            None
        }
    };
    match queued {
        Some(message) => {
            // Delivery takes the message out of the inbox, so the sender's
            // user gets the queue charge back (issue #103).
            release_queued_quota(message.quota_uid, message.bytes.len());
            Ok(Some(deliver(message)?))
        }
        None => Ok(None),
    }
}

/// Install a queued message's transfers into the receiving task's handle table
/// and rewrite them to local numbers.
///
/// On failure (the receiver is out of handles) everything installed is rolled
/// back and the references of everything still pending are released, so a
/// failed delivery cannot leak handles or frames.
pub(super) fn deliver(queued: Queued) -> Result<Message, Error> {
    let mut handles_out: Vec<u64> = Vec::with_capacity(queued.handles.len());
    let mut buffers_out: Vec<BufferDesc> = Vec::with_capacity(queued.buffers.len());
    for (index, transfer) in queued.handles.iter().enumerate() {
        let opened = if transfer.kind == HandleKind::Buffer {
            shared::attach(transfer.object_id, transfer.rights).map_err(from_shared)
        } else {
            handles::open(transfer.kind, transfer.rights, transfer.object_id).map_err(from_handles)
        };
        match opened {
            Ok(handle) => handles_out.push(handle),
            Err(error) => {
                rollback_delivery(&queued, index, &handles_out, &buffers_out);
                return Err(error);
            }
        }
    }
    for buffer in queued.buffers.iter() {
        match shared::attach(buffer.object_id, buffer.rights) {
            Ok(handle) => buffers_out.push(BufferDesc {
                handle,
                offset: buffer.offset,
                len: buffer.len,
                flags: buffer.flags,
            }),
            Err(error) => {
                rollback_delivery(&queued, queued.handles.len(), &handles_out, &buffers_out);
                return Err(from_shared(error));
            }
        }
    }
    Ok(Message {
        sender: queued.sender,
        method: queued.method,
        flags: queued.flags,
        txn: queued.txn,
        deadline: queued.deadline,
        bytes: queued.bytes,
        handles: handles_out,
        buffers: buffers_out,
    })
}

/// Undo a partial [`deliver`]: close the installed handles and release the
/// message references of everything still pending.
///
/// Non-buffer objects have no kernel object refcount yet, so releasing a moved
/// handle whose delivery failed drops the handle but not the object; that is
/// the documented follow-up for when `HandleEntry` grows a refcount.
pub(super) fn rollback_delivery(
    queued: &Queued,
    installed: usize,
    handles_out: &[u64],
    buffers_out: &[BufferDesc],
) {
    for (transfer, &handle) in queued.handles[..installed].iter().zip(handles_out) {
        if transfer.kind == HandleKind::Buffer {
            shared::close(handle).ok();
        } else {
            handles::close(handle).ok();
        }
    }
    for transfer in &queued.handles[installed..] {
        if transfer.kind == HandleKind::Buffer {
            shared::release(transfer.object_id);
        }
    }
    for descriptor in buffers_out {
        // Closing the receiver's buffer handle drops the reference `attach`
        // converted from the message.
        shared::close(descriptor.handle).ok();
    }
    for buffer in &queued.buffers[buffers_out.len()..] {
        shared::release(buffer.object_id);
    }
}

/// Receive the next message, parking until one arrives, the deadline passes, or
/// the peer closes.
pub fn recv(handle: u64, deadline: Option<u64>) -> Result<Message, Error> {
    loop {
        match try_recv(handle) {
            Ok(Some(message)) => return Ok(message),
            Ok(None) => {}
            Err(error) => return Err(error),
        }
        let reason = MESSENGER.wait(task::current(), deadline);
        if reason == WakeReason::TimedOut {
            return Err(Error::TimedOut);
        }
    }
}
