//! Parcel encode/decode helpers for the topics broker protocol.

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{Decoder, Encoder, Kind, Parcel};

use super::super::{errno, Error, Result};
use super::{field, header, method, Event, Qos, SubscriptionStats, TopicInfo};

/// Wrap an encoded body in a broker parcel.
pub fn request_parcel(method: u32, body: Encoder) -> Parcel {
    Parcel {
        header: header(method),
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    }
}

/// An empty reply.
pub fn reply_ok(method: u32) -> Parcel {
    request_parcel(method, Encoder::new())
}

/// A publish reply carrying the subscriber count the event reached.
pub fn reply_matched(matched: u64) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::MATCHED, matched).map_err(Error::Parcel)?;
    Ok(request_parcel(method::PUBLISH, body))
}

/// A subscribe reply carrying the new subscription id.
pub fn reply_subscription(id: u64) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::SUBSCRIPTION, id).map_err(Error::Parcel)?;
    Ok(request_parcel(method::SUBSCRIBE, body))
}

/// A `NextEvent` reply carrying one event.
pub fn reply_event(event: &Event) -> Result<Parcel> {
    let mut record = Encoder::new();
    record
        .string(field::TOPIC, &event.topic)
        .map_err(Error::Parcel)?;
    record
        .u64(field::PUBLISHER, event.publisher)
        .map_err(Error::Parcel)?;
    record
        .u64(field::SEQUENCE, event.sequence)
        .map_err(Error::Parcel)?;
    record
        .bool(field::RETAINED, event.retained)
        .map_err(Error::Parcel)?;
    record
        .bytes(field::PAYLOAD, &event.payload)
        .map_err(Error::Parcel)?;
    let mut body = Encoder::new();
    body.record(field::EVENT, &record).map_err(Error::Parcel)?;
    Ok(request_parcel(method::NEXT_EVENT, body))
}

/// A stats reply.
pub fn reply_stats(stats: &SubscriptionStats) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u32(field::QOS, stats.qos).map_err(Error::Parcel)?;
    body.u32(field::DEPTH, stats.depth).map_err(Error::Parcel)?;
    body.u64(field::QUEUED, stats.queued)
        .map_err(Error::Parcel)?;
    body.u64(field::DELIVERED, stats.delivered)
        .map_err(Error::Parcel)?;
    body.u64(field::MATCHED, stats.matched)
        .map_err(Error::Parcel)?;
    body.u64(field::DROPS, stats.drops).map_err(Error::Parcel)?;
    Ok(request_parcel(method::STATS, body))
}

/// A topic-list reply.
pub fn reply_topics(topics: &[TopicInfo]) -> Result<Parcel> {
    let mut body = Encoder::new();
    for info in topics {
        let mut record = Encoder::new();
        record
            .string(field::TOPIC, &info.topic)
            .map_err(Error::Parcel)?;
        record
            .u64(field::SUBSCRIBERS, info.subscribers)
            .map_err(Error::Parcel)?;
        record
            .bool(field::RETAINED, info.retained)
            .map_err(Error::Parcel)?;
        body.record(field::ENTRY, &record).map_err(Error::Parcel)?;
    }
    Ok(request_parcel(method::LIST_TOPICS, body))
}

/// The broker's error answer: errno-style code plus friendly text.
pub fn error_reply(method: u32, error: Error) -> Parcel {
    let code = error.errno().map(|code| -code).unwrap_or(errno::EINVAL);
    let mut body = Encoder::new();
    // A structured error field cannot overflow a fresh encoder here.
    let _ = body.error(field::ERROR, code as u32, error.message());
    request_parcel(method, body)
}

/// The first structured error field, when the reply is a broker failure.
///
/// `pub(super)` so [`super::client`] can check every reply for a daemon
/// failure without re-decoding the parcel.
pub(super) fn error_field(parcel: &Parcel) -> Result<Option<i64>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::Error && field.id == field::ERROR {
            let (code, _message) = field.error_parts().map_err(Error::Parcel)?;
            return Ok(Some(code as i64));
        }
    }
    Ok(None)
}

/// The first string field with `id`.
pub fn string_field(parcel: &Parcel, id: u16) -> Result<String> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::String && field.id == id {
            return Ok(String::from(field.as_str().map_err(Error::Parcel)?));
        }
    }
    Err(Error::Errno(-errno::EINVAL))
}

/// The first `u64` field with `id`, if any.
pub fn u64_field(parcel: &Parcel, id: u16) -> Result<Option<u64>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::U64 && field.id == id {
            return Ok(Some(field.as_u64().map_err(Error::Parcel)?));
        }
    }
    Ok(None)
}

/// The first `u32` field with `id`, if any.
pub fn u32_field(parcel: &Parcel, id: u16) -> Result<Option<u32>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::U32 && field.id == id {
            return Ok(Some(field.as_u32().map_err(Error::Parcel)?));
        }
    }
    Ok(None)
}

/// The first `bool` field with `id` (default `false`).
pub fn bool_field(parcel: &Parcel, id: u16) -> Result<bool> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::Bool && field.id == id {
            return field.as_bool().map_err(Error::Parcel);
        }
    }
    Ok(false)
}

/// The first `Bytes` field with `id`, if any.
pub fn bytes_field(parcel: &Parcel, id: u16) -> Result<Option<Vec<u8>>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::Bytes && field.id == id {
            return Ok(Some(field.as_bytes().to_vec()));
        }
    }
    Ok(None)
}

/// Decode the first nested `EVENT` record, if the reply carries one.
pub fn decode_event(parcel: &Parcel) -> Result<Option<Event>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(record) = decoder.next().map_err(Error::Parcel)? {
        if record.kind != Kind::Struct || record.id != field::EVENT {
            continue;
        }
        let mut nested = record.nested(0).map_err(Error::Parcel)?;
        let mut event = Event {
            topic: String::new(),
            publisher: 0,
            sequence: 0,
            retained: false,
            payload: Vec::new(),
        };
        while let Some(item) = nested.next().map_err(Error::Parcel)? {
            match (item.kind, item.id) {
                (Kind::String, field::TOPIC) => {
                    event.topic = String::from(item.as_str().map_err(Error::Parcel)?);
                }
                (Kind::U64, field::PUBLISHER) => {
                    event.publisher = item.as_u64().map_err(Error::Parcel)?;
                }
                (Kind::U64, field::SEQUENCE) => {
                    event.sequence = item.as_u64().map_err(Error::Parcel)?;
                }
                (Kind::Bool, field::RETAINED) => {
                    event.retained = item.as_bool().map_err(Error::Parcel)?;
                }
                (Kind::Bytes, field::PAYLOAD) => {
                    event.payload = item.as_bytes().to_vec();
                }
                _ => {}
            }
        }
        return Ok(Some(event));
    }
    Ok(None)
}

/// Decode a stats reply.
pub fn decode_stats(parcel: &Parcel) -> Result<SubscriptionStats> {
    Ok(SubscriptionStats {
        qos: u32_field(parcel, field::QOS)?.unwrap_or(0),
        depth: u32_field(parcel, field::DEPTH)?.unwrap_or(0),
        queued: u64_field(parcel, field::QUEUED)?.unwrap_or(0),
        delivered: u64_field(parcel, field::DELIVERED)?.unwrap_or(0),
        matched: u64_field(parcel, field::MATCHED)?.unwrap_or(0),
        drops: u64_field(parcel, field::DROPS)?.unwrap_or(0),
    })
}

/// Decode a topic-list reply.
pub fn decode_topics(parcel: &Parcel) -> Result<Vec<TopicInfo>> {
    let mut topics = Vec::new();
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(record) = decoder.next().map_err(Error::Parcel)? {
        if record.kind != Kind::Struct || record.id != field::ENTRY {
            continue;
        }
        let mut nested = record.nested(0).map_err(Error::Parcel)?;
        let mut info = TopicInfo {
            topic: String::new(),
            subscribers: 0,
            retained: false,
        };
        while let Some(item) = nested.next().map_err(Error::Parcel)? {
            match (item.kind, item.id) {
                (Kind::String, field::TOPIC) => {
                    info.topic = String::from(item.as_str().map_err(Error::Parcel)?);
                }
                (Kind::U64, field::SUBSCRIBERS) => {
                    info.subscribers = item.as_u64().map_err(Error::Parcel)?;
                }
                (Kind::Bool, field::RETAINED) => {
                    info.retained = item.as_bool().map_err(Error::Parcel)?;
                }
                _ => {}
            }
        }
        topics.push(info);
    }
    Ok(topics)
}

/// Encode a `Publish` request body.
///
/// `pub(super)` so [`super::client`] can build the request without
/// duplicating the field layout.
pub(super) fn publish_body(topic: &str, payload: &[u8], retained: bool) -> Result<Encoder> {
    let mut body = Encoder::new();
    body.string(field::TOPIC, topic).map_err(Error::Parcel)?;
    body.bytes(field::PAYLOAD, payload).map_err(Error::Parcel)?;
    body.bool(field::RETAINED, retained)
        .map_err(Error::Parcel)?;
    Ok(body)
}

/// Encode a `Subscribe` request body.
pub(super) fn subscribe_body(filter: &str, qos: Qos) -> Result<Encoder> {
    let mut body = Encoder::new();
    body.string(field::FILTER, filter).map_err(Error::Parcel)?;
    body.u32(field::QOS, qos.code()).map_err(Error::Parcel)?;
    body.u32(field::DEPTH, qos.depth()).map_err(Error::Parcel)?;
    Ok(body)
}

/// Encode a request body that names one subscription.
pub(super) fn subscription_body(id: u64) -> Result<Encoder> {
    let mut body = Encoder::new();
    body.u64(field::SUBSCRIPTION, id).map_err(Error::Parcel)?;
    Ok(body)
}
