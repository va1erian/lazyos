//! Self-test of the user side of objects as fields (`docs/messenger-core-plan.md`
//! 3.3): a message owns what the kernel installed for it until a decoder
//! claims it, a decoder refuses an index that is not the declared slot, and
//! whatever stays unclaimed is closed when the message drops.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{flags, Encoder, Header, Kind, Object, Parcel, VERSION};
use messenger_generated::os_lazy_init_app_v1 as app;
use user::messenger::{self, create_pair, fabric_stats, Endpoint, Message};
use user::sys;

/// `MESSAGE:OBJECTS`: every rule, in one pass.
pub(crate) fn selftest_objects() -> Result<(), String> {
    unclaimed_channel_is_released_on_drop()?;
    unclaimed_buffer_is_closed_on_drop()?;
    claimed_channel_belongs_to_the_decoder()?;
    bad_index_is_refused_and_the_object_still_closes()?;
    Ok(())
}

/// A `Watch` request (one declared channel) whose `events` field carries
/// `index` instead of the declared slot 0.
fn watch_request(end: u64, index: u32) -> Result<Parcel, String> {
    let mut body = Encoder::new();
    body.raw(Kind::Handle, 1, &index.to_le_bytes())
        .map_err(|error| String::from(error.message()))?;
    Ok(Parcel {
        header: Header {
            version: VERSION,
            flags: flags::ONE_WAY,
            interface_id: app::INTERFACE_ID,
            method: app::METHOD_WATCH,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        objects: alloc::vec![Object::Channel(end)],
    })
}

/// Send `parcel` to ourselves over a fresh pair and receive it.
fn round_trip(parcel: &Parcel) -> Result<(Message, Endpoint, Endpoint), String> {
    let (sender, receiver) = create_pair().map_err(|error| format!("create_pair: {error:?}"))?;
    sender
        .send(parcel)
        .map_err(|error| format!("send: {error:?}"))?;
    let message = receiver
        .poll_recv()
        .map_err(|error| format!("recv: {error:?}"))?
        .ok_or("the message never arrived")?;
    Ok((message, sender, receiver))
}

/// Whether `kept`, the peer of a moved end, sees that end closed.
fn peer_closed(kept: &Endpoint) -> bool {
    matches!(kept.poll_recv(), Err(messenger::Error::Errno(code)) if code == -messenger::errno::EPIPE)
}

fn unclaimed_channel_is_released_on_drop() -> Result<(), String> {
    let (kept, moved) = create_pair().map_err(|error| format!("create_pair: {error:?}"))?;
    let (message, sender, receiver) = round_trip(&watch_request(moved.handle(), 0)?)?;
    if message.objects().len() != 1 {
        return Err(format!(
            "delivered {} objects, expected 1",
            message.objects().len()
        ));
    }
    if peer_closed(&kept) {
        return Err("the end closed while the message still held it".into());
    }
    drop(message);
    if !peer_closed(&kept) {
        return Err("an unclaimed channel end was not released on drop".into());
    }
    let _ = sender.close();
    let _ = receiver.close();
    let _ = kept.close();
    Ok(())
}

fn unclaimed_buffer_is_closed_on_drop() -> Result<(), String> {
    let live = || {
        fabric_stats()
            .map(|stats| stats.buffers)
            .map_err(|error| format!("stats: {error:?}"))
    };
    let before = live()?;
    let (buffer, _, _) =
        sys::buffer_create(4096).map_err(|code| format!("buffer_create: {code}"))?;
    let mut body = Encoder::new();
    let mut objects = Vec::new();
    body.buffer(1, &libmessenger::Buffer::whole(buffer, 4096), &mut objects)
        .map_err(|error| String::from(error.message()))?;
    // `AttachKeyState(session, state: Buffer)`: one declared buffer.
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: flags::ONE_WAY,
            interface_id: messenger_generated::os_lazy_input_v1::INTERFACE_ID,
            method: messenger_generated::os_lazy_input_v1::METHOD_ATTACHKEYSTATE,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        objects,
    };
    let (message, sender, receiver) = round_trip(&parcel)?;
    // Our own handle goes first: the message's is the buffer's last holder.
    sys::buffer_close(buffer).map_err(|code| format!("buffer_close: {code}"))?;
    if live()? != before + 1 {
        return Err("the message's reference did not keep the buffer alive".into());
    }
    drop(message);
    if live()? != before {
        return Err(format!(
            "an unclaimed buffer was not closed on drop: {} live, had {before}",
            live()?
        ));
    }
    let _ = sender.close();
    let _ = receiver.close();
    Ok(())
}

fn claimed_channel_belongs_to_the_decoder() -> Result<(), String> {
    let (kept, moved) = create_pair().map_err(|error| format!("create_pair: {error:?}"))?;
    let (message, sender, receiver) = round_trip(&watch_request(moved.handle(), 0)?)?;
    let args = message
        .decode(app::decode_watch_args)
        .map_err(|error| format!("decode: {error:?}"))?;
    if !message.objects().is_empty() {
        return Err("the message still holds a claimed object".into());
    }
    drop(message);
    if peer_closed(&kept) {
        return Err("dropping the message closed an end its decoder claimed".into());
    }
    let _ = Endpoint::from_raw(args.events).close();
    if !peer_closed(&kept) {
        return Err("closing the claimed end was not seen by its peer".into());
    }
    let _ = sender.close();
    let _ = receiver.close();
    let _ = kept.close();
    Ok(())
}

fn bad_index_is_refused_and_the_object_still_closes() -> Result<(), String> {
    let (kept, moved) = create_pair().map_err(|error| format!("create_pair: {error:?}"))?;
    // The kernel accepts the list (one channel, as declared); the field's
    // index names slot 7, which the decoder refuses.
    let (message, sender, receiver) = round_trip(&watch_request(moved.handle(), 7)?)?;
    match message.decode(app::decode_watch_args) {
        Err(messenger::Error::Parcel(libmessenger::Error::BadObjectIndex)) => {}
        other => return Err(format!("a bad index decoded as {other:?}")),
    }
    if message.objects().len() != 1 {
        return Err("a refused decode took the object".into());
    }
    drop(message);
    if !peer_closed(&kept) {
        return Err("the object of a refused decode was not released on drop".into());
    }
    let _ = sender.close();
    let _ = receiver.close();
    let _ = kept.close();
    Ok(())
}
