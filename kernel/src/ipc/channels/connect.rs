//! Per-connection channels (issue #483).
//!
//! A resolved name hands every client a handle to the *same* endpoint, so one
//! client closing it is peer death for all of them. A connection is a fresh
//! channel per client instead: the client keeps side 0, and side 1 is moved to
//! the service as a kernel-posted `Connected` message on its registered
//! endpoint, stamped with the client's identity. Either party closing ends
//! that connection only.
//!
//! The service end travels like any moved handle. If the service never takes
//! the notice (its endpoint closes first), the end has no holder and is closed
//! by the orphan pass ([`close_orphans`]), so the client's calls fail with
//! `PeerDied` rather than wait.

use super::*;

/// Open a connection to the endpoint a name registered (`registered` is the
/// object id of the side its clients resolve): a new channel whose side 0
/// opens in `client`'s table with `client_rights`, and whose side 1 reaches
/// the service in a one-way message carrying `notice` (an encoded
/// `Connected` parcel). Returns the client's handle.
///
/// Refused before anything is created when the service side is closed; any
/// later failure (the client's table, the service's queue or the client's
/// queue quota) undoes the channel and the client's handle.
pub fn connect(
    registered: u64,
    client: usize,
    client_rights: u32,
    notice: Vec<u8>,
) -> Result<u64, Error> {
    let (listen_id, client_side) = split_object_id(registered);
    {
        let channels = CHANNELS.lock();
        let channel = find_channel_ref(&channels, listen_id)?;
        if channel.endpoints[1 - client_side].closed {
            return Err(Error::PeerDied);
        }
    }
    let parcel = validate_parcel(&notice)?;
    let (method, parcel_flags) = (parcel.header.method, parcel.header.flags);
    let channel_id = CHANNELS.lock().insert(fresh_channel)?;
    let undo_channel = || {
        CHANNELS.lock().remove(channel_id);
    };
    let handle = match handles::open_for_task(
        client,
        HandleKind::Channel,
        client_rights,
        object_id(channel_id, 0),
    ) {
        Ok(handle) => handle,
        Err(error) => {
            undo_channel();
            return Err(from_handles(error));
        }
    };
    let queued = Queued {
        sender: client,
        origin: SenderId::of(client),
        method,
        flags: parcel_flags,
        txn: None,
        deadline: None,
        bytes: notice,
        handles: alloc::vec![Transfer {
            kind: HandleKind::Channel,
            rights: rights::ALL,
            object_id: object_id(channel_id, 1),
        }],
        buffers: Vec::new(),
    };
    // `enqueue` delivers to the peer of the side it is given: the service.
    match enqueue(listen_id, client_side, queued) {
        Ok(receivers) => {
            wake(receivers.iter());
            ring(listen_id, 1 - client_side);
            Ok(handle)
        }
        Err(error) => {
            handles::close_for_task(client, handle).ok();
            undo_channel();
            Err(error)
        }
    }
}

/// A channel with two open, empty endpoints (shared with [`super::create`]).
pub(super) fn fresh_channel(id: u64) -> Channel {
    Channel {
        id,
        endpoints: [Endpoint::default(), Endpoint::default()],
        txns: Vec::new(),
        senders: Vec::new(),
        calls: 0,
        replies: 0,
        timeouts: 0,
        cancels: 0,
        drops: 0,
    }
}
