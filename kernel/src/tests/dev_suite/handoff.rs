//! A DMA buffer's trip from a driver task to a client task: a channel
//! mirrored into the client's table, a one-way parcel that shares the buffer
//! and drops the driver's handle, and the receive on the other side.

use super::fixture::*;
use super::*;
use libmessenger::{flags, Encoder, Header, Parcel, VERSION};

/// Open a channel in the calling task, then mirror the receiving endpoint into
/// `slot`'s table. Returns the caller's sender handle and the receiver's
/// mirror (both name the same channel).
pub fn channel_to(slot: usize) -> Result<(u64, u64), String> {
    let (client, server) = channels::create().map_err(|error| error.message().to_string())?;
    let entry = handles::get(server).map_err(|error| error.message().to_string())?;
    let caller = task::current();
    task::harness::switch_current(slot);
    let mirror = handles::open(handles::HandleKind::Channel, entry.rights, entry.object_id)
        .map_err(|error| error.message().to_string())?;
    task::harness::switch_current(caller);
    Ok((client, mirror))
}

/// Build and send a one-way parcel that shares `buffer` with the endpoint's
/// peer, then close the sender's handle (and its mapping): the message holds
/// the only reference until the peer receives it, as a moved handle once did.
pub fn send_buffer(endpoint: u64, buffer: u64) -> Result<(), String> {
    let size = crate::ipc::shared::info(buffer)
        .map_err(|error| error.message().to_string())?
        .size;
    let mut body = Encoder::new();
    body.u64(1, 7).map_err(|error| error.message())?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: flags::ONE_WAY,
            interface_id: 0x0bad_cafe,
            method: 1,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles: Vec::new(),
        buffers: vec![libmessenger::BufferDesc {
            handle: buffer,
            offset: 0,
            len: size,
            flags: 0,
        }],
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|error| error.message())?;
    channels::send(endpoint, &bytes).map_err(|error| error.message().to_string())?;
    crate::ipc::shared::close(buffer).map_err(|error| error.message().to_string())
}

/// Transfer `buffer` from `from` (the current task) to `to`, receive it there,
/// close it, and return to `from`. Used to keep a long-lived client from
/// accumulating handles across a soak.
pub fn transfer_and_consume(from: usize, to: usize, buffer: u64) -> Result<(), String> {
    let (endpoint, server) = channel_to(to)?;
    send_buffer(endpoint, buffer)?;
    task::harness::switch_current(to);
    mem::switch_to(PhysAddr::new(task::harness::pml4(to).ok_or("no table")?));
    let message = channels::try_recv(server)
        .map_err(|error| error.message().to_string())?
        .ok_or("the transferred buffer never arrived")?;
    check!(
        message.buffers.len() == 1,
        "the transfer delivered {} buffers",
        message.buffers.len()
    );
    crate::ipc::shared::close(message.buffers[0].handle)
        .map_err(|error| error.message().to_string())?;
    channels::close_endpoint(server).map_err(|error| error.message().to_string())?;
    task::harness::switch_current(from);
    mem::switch_to(PhysAddr::new(task::harness::pml4(from).ok_or("no table")?));
    Ok(())
}

/// [`transfer_and_consume`] without the close: returns the handle the client
/// now holds (in the client table), so the buffer outlives its sender.
pub fn transfer_and_hold(from: usize, to: usize, buffer: u64) -> Result<u64, String> {
    let (endpoint, server) = channel_to(to)?;
    send_buffer(endpoint, buffer)?;
    task::harness::switch_current(to);
    mem::switch_to(PhysAddr::new(task::harness::pml4(to).ok_or("no table")?));
    let message = channels::try_recv(server)
        .map_err(|error| error.message().to_string())?
        .ok_or("the transferred buffer never arrived")?;
    check!(
        message.buffers.len() == 1,
        "the transfer delivered no buffer"
    );
    let held = message.buffers[0].handle;
    channels::close_endpoint(server).map_err(|error| error.message().to_string())?;
    task::harness::switch_current(from);
    mem::switch_to(PhysAddr::new(task::harness::pml4(from).ok_or("no table")?));
    Ok(held)
}
