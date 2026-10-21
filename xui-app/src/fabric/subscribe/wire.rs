//! Request building and reply decoding for the topics broker, on the
//! `midlc`-generated stubs. Everything here is pure: bytes in, values out, so
//! the client logic and its fuzz tests need no kernel.

use libmessenger::{flags, Header, Parcel, VERSION};
use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::os_lazy_messenger_topics_bell_v1 as bell;
use messenger_generated::os_lazy_messenger_topics_v1 as wire;

use super::{Errno, Qos, RawEvent, SubId, SubStats, EINVAL};

/// Encode a request parcel; a parcel the codec refuses is `-EINVAL`.
pub(super) fn encode(parcel: &Parcel) -> Result<Vec<u8>, Errno> {
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|_| -EINVAL)?;
    Ok(bytes)
}

/// A header for a call on the topics interface. `ALLOW_NESTED` is required:
/// every client shares the broker's channel, so a call may overlap another
/// task's.
fn header(method: u32) -> Header {
    Header {
        version: VERSION,
        flags: flags::SYNC | flags::ALLOW_NESTED,
        interface_id: wire::INTERFACE_ID,
        method,
        txn_id: 0,
        reply_to: 0,
        deadline_ns: 0,
    }
}

fn request(method: u32, body: Vec<u8>, objects: Vec<libmessenger::Object>) -> Parcel {
    Parcel {
        header: header(method),
        body,
        objects,
    }
}

/// `Subscribe`: `depth` is clamped to the broker's `1..=64`.
pub(super) fn subscribe_request(filter: &str, qos: Qos, depth: u32) -> Result<Parcel, Errno> {
    let args = wire::SubscribeArgs {
        filter: filter.into(),
        qos: qos.code(),
        depth: depth.clamp(1, Qos::MAX_DEPTH),
    };
    let body = wire::encode_subscribe_args(&args).map_err(|_| -EINVAL)?;
    Ok(request(wire::METHOD_SUBSCRIBE, body, Vec::new()))
}

/// `Unsubscribe`, `NextEvent` or `Stats` (`method`): all three take only the
/// subscription id, with one layout.
pub(super) fn by_id_request(method: u32, id: SubId) -> Result<Parcel, Errno> {
    let args = wire::UnsubscribeArgs { subscription: id.0 };
    let body = wire::encode_unsubscribe_args(&args).map_err(|_| -EINVAL)?;
    Ok(request(method, body, Vec::new()))
}

/// `Ack`: retire every event up to `sequence`.
pub(super) fn ack_request(id: SubId, sequence: u64) -> Result<Parcel, Errno> {
    let args = wire::AckArgs {
        subscription: id.0,
        sequence,
    };
    let body = wire::encode_ack_args(&args).map_err(|_| -EINVAL)?;
    Ok(request(wire::METHOD_ACK, body, Vec::new()))
}

/// `Bell`: carries `handle`, the channel end the broker will ring on (the
/// kernel moves it out of this task's table when the call is sent).
pub(super) fn bell_request(id: SubId, handle: u64) -> Result<Parcel, Errno> {
    let args = wire::BellArgs {
        subscription: id.0,
        bell: handle,
    };
    let (body, objects) = wire::encode_bell_args(&args).map_err(|_| -EINVAL)?;
    Ok(request(wire::METHOD_BELL, body, objects))
}

/// The negative errno of a structured error field in a reply, if it has one.
/// A code of zero is still a failure, reported as `-EINVAL`.
fn reply_error(parcel: &Parcel) -> Option<Errno> {
    let mut decoder = libmessenger::Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == libmessenger::Kind::Error && field.id == ERROR_FIELD {
            return Some(match field.error_parts() {
                Ok((code, _)) if code != 0 => -i64::from(code),
                _ => -EINVAL,
            });
        }
    }
    None
}

/// Decode reply bytes and turn a broker error field into `Err`.
pub(super) fn parse_reply(bytes: &[u8]) -> Result<Parcel, Errno> {
    let parcel = Parcel::decode(bytes).map_err(|_| -EINVAL)?;
    match reply_error(&parcel) {
        Some(code) => Err(code),
        None => Ok(parcel),
    }
}

/// The new subscription id of a `Subscribe` reply. Ids start at 1, so zero
/// is the absent-field default of a malformed reply.
pub(super) fn decode_subscription(parcel: &Parcel) -> Result<SubId, Errno> {
    let reply = wire::decode_subscribe_reply(&parcel.body).map_err(|_| -EINVAL)?;
    match reply.subscription {
        0 => Err(-EINVAL),
        id => Ok(SubId(id)),
    }
}

/// The event of a `NextEvent` reply; `None` for the empty reply. An event
/// without a topic is no event a broker sends, so it is refused.
pub(super) fn decode_event(parcel: &Parcel) -> Result<Option<RawEvent>, Errno> {
    if parcel.body.is_empty() {
        return Ok(None);
    }
    let event = wire::decode_next_event_reply(&parcel.body)
        .map_err(|_| -EINVAL)?
        .event;
    if event.topic.is_empty() {
        return Err(-EINVAL);
    }
    Ok(Some(RawEvent {
        topic: event.topic,
        publisher: event.publisher,
        sequence: event.sequence,
        retained: event.retained,
        payload: event.payload,
    }))
}

/// A `Stats` reply.
pub(super) fn decode_stats(parcel: &Parcel) -> Result<SubStats, Errno> {
    let stats = wire::decode_stats_reply(&parcel.body)
        .map_err(|_| -EINVAL)?
        .stats;
    Ok(SubStats {
        qos: stats.qos,
        depth: stats.depth,
        queued: stats.queued,
        delivered: stats.delivered,
        matched: stats.matched,
        drops: stats.drops,
    })
}

/// The subscription a received bell message names; `None` unless it is a
/// well-formed `Ready` on the bell interface.
pub(super) fn ready_subscription(bytes: &[u8]) -> Option<SubId> {
    let parcel = Parcel::decode(bytes).ok()?;
    if parcel.header.interface_id != bell::INTERFACE_ID
        || parcel.header.method != bell::METHOD_READY
    {
        return None;
    }
    match bell::decode_ready_args(&parcel.body).ok()?.subscription {
        0 => None,
        id => Some(SubId(id)),
    }
}
