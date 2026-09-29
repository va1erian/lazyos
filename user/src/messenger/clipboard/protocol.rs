//! Parcel encode/decode helpers for the clipboard protocol.

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{Decoder, Encoder, Field, Kind, Parcel};

use super::super::{errno, router, Error, Result};
use super::{
    field, method, parcel, BufferHandle, OfferInfo, OfferRequest, INTERFACE, OWNER_INTERFACE,
    READ_INTERFACE, WRITE_INTERFACE,
};

/// `Offer(owner, mime_types) -> token` for an eager offer: the payloads
/// ride along and the service keeps one bounded copy.
pub fn offer_request(owner: &str, offers: &[(&str, &[u8])]) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::OWNER, owner).map_err(Error::Parcel)?;
    let mut mimes = Encoder::new();
    let mut data = Encoder::new();
    for (mime, bytes) in offers {
        mimes.string(field::MIMES, mime).map_err(Error::Parcel)?;
        let mut record = Encoder::new();
        record.string(field::MIME, mime).map_err(Error::Parcel)?;
        record.bytes(field::BYTES, bytes).map_err(Error::Parcel)?;
        data.record(field::DATA, &record).map_err(Error::Parcel)?;
    }
    body.array(field::MIMES, &mimes).map_err(Error::Parcel)?;
    body.array(field::DATA, &data).map_err(Error::Parcel)?;
    Ok(parcel(WRITE_INTERFACE, method::OFFER, body))
}

/// `Offer` for a lazy offer: only the MIME list crosses the wire. `sink`
/// names the registry entry where the owner serves [`method::SERIALIZE`]
/// when a paste actually happens.
pub fn offer_lazy_request(owner: &str, sink: &str, mimes: &[&str]) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::OWNER, owner).map_err(Error::Parcel)?;
    body.string(field::SINK, sink).map_err(Error::Parcel)?;
    let mut array = Encoder::new();
    for mime in mimes {
        array.string(field::MIMES, mime).map_err(Error::Parcel)?;
    }
    body.array(field::MIMES, &array).map_err(Error::Parcel)?;
    Ok(parcel(WRITE_INTERFACE, method::OFFER, body))
}

/// An `Offer` reply carrying the new token.
pub fn token_reply(token: u64) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::TOKEN, token).map_err(Error::Parcel)?;
    Ok(parcel(WRITE_INTERFACE, method::OFFER, body))
}

/// `Request(token, mime)`; `token == 0` selects the newest offer in the
/// caller's session that lists `mime`.
pub fn request_request(token: u64, mime: &str) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::TOKEN, token).map_err(Error::Parcel)?;
    body.string(field::MIME, mime).map_err(Error::Parcel)?;
    Ok(parcel(READ_INTERFACE, method::REQUEST, body))
}

/// A `Request` reply carrying the payload (the `BufferHandle` shape).
pub fn request_reply(handle: &BufferHandle) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::TOKEN, handle.token)
        .map_err(Error::Parcel)?;
    body.string(field::MIME, &handle.mime)
        .map_err(Error::Parcel)?;
    body.bool(field::LAZY, handle.lazy).map_err(Error::Parcel)?;
    body.bytes(field::BYTES, &handle.bytes)
        .map_err(Error::Parcel)?;
    Ok(parcel(READ_INTERFACE, method::REQUEST, body))
}

/// `Serialize(token, mime)`: the service calls this on a lazy offer's owner
/// endpoint when a paste happens.
pub fn serialize_request(token: u64, mime: &str) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::TOKEN, token).map_err(Error::Parcel)?;
    body.string(field::MIME, mime).map_err(Error::Parcel)?;
    Ok(parcel(OWNER_INTERFACE, method::SERIALIZE, body))
}

/// The owner's `Serialize` answer.
pub fn serialize_reply(bytes: &[u8]) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.bytes(field::BYTES, bytes).map_err(Error::Parcel)?;
    Ok(parcel(OWNER_INTERFACE, method::SERIALIZE, body))
}

/// A `Ping` request.
pub fn ping_request() -> Parcel {
    parcel(INTERFACE, method::PING, Encoder::new())
}

/// A `Current` request (offer metadata only; never content).
pub fn current_request() -> Parcel {
    parcel(INTERFACE, method::CURRENT, Encoder::new())
}

/// An empty successful reply on `interface_id`/`method`.
pub fn ok_reply(interface_id: u64, method: u32) -> Parcel {
    parcel(interface_id, method, Encoder::new())
}

/// Encode `info` into a `Current` reply; `None` when no offer is live.
pub fn current_reply(info: Option<&OfferInfo>) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.u64(field::FOUND, info.is_some() as u64)
        .map_err(Error::Parcel)?;
    if let Some(info) = info {
        body.record(field::OFFER, &info_body(info)?)
            .map_err(Error::Parcel)?;
    }
    Ok(parcel(INTERFACE, method::CURRENT, body))
}

/// Encode an offer's metadata as the retained `.../clipboard/changed`
/// event payload: a parcel with the `OFFER` record, never content.
pub fn changed_payload(info: &OfferInfo) -> Result<Vec<u8>> {
    let mut body = Encoder::new();
    body.record(field::OFFER, &info_body(info)?)
        .map_err(Error::Parcel)?;
    let mut bytes = Vec::new();
    parcel(INTERFACE, method::CURRENT, body)
        .encode(&mut bytes)
        .map_err(Error::Parcel)?;
    Ok(bytes)
}

/// The offer-metadata body shared by `Current` and the changed event.
fn info_body(info: &OfferInfo) -> Result<Encoder> {
    let mut body = Encoder::new();
    body.u64(field::TOKEN, info.token).map_err(Error::Parcel)?;
    body.string(field::OWNER, &info.owner)
        .map_err(Error::Parcel)?;
    body.u64(field::SESSION, info.session)
        .map_err(Error::Parcel)?;
    body.bool(field::LAZY, info.lazy).map_err(Error::Parcel)?;
    body.u64(field::TICK, info.tick).map_err(Error::Parcel)?;
    let mut array = Encoder::new();
    for mime in &info.mimes {
        array.string(field::MIMES, mime).map_err(Error::Parcel)?;
    }
    body.array(field::MIMES, &array).map_err(Error::Parcel)?;
    Ok(body)
}

/// The service's error answer: errno-style code plus friendly text.
pub fn error_reply(interface_id: u64, method: u32, error: Error) -> Parcel {
    let code = error.errno().map(|code| -code).unwrap_or(errno::EINVAL);
    let mut body = Encoder::new();
    // A structured error field cannot overflow a fresh encoder here.
    let _ = body.error(field::ERROR, code as u32, error.message());
    parcel(interface_id, method, body)
}

/// The first structured error field, when the reply is a service failure.
///
/// `pub(super)` so [`super::client`] can check every reply for a service
/// failure without re-decoding the parcel.
pub(super) fn error_field(parcel: &Parcel) -> Result<Option<i64>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(item) = decoder.next().map_err(Error::Parcel)? {
        if item.kind == Kind::Error && item.id == field::ERROR {
            let (code, _message) = item.error_parts().map_err(Error::Parcel)?;
            return Ok(Some(code as i64));
        }
    }
    Ok(None)
}

/// Decode an `Offer` request into its owner, sink, MIME list and payloads.
pub fn decode_offer(parcel: &Parcel) -> Result<OfferRequest> {
    let mut request = OfferRequest::default();
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(item) = decoder.next().map_err(Error::Parcel)? {
        match (item.kind, item.id) {
            (Kind::String, field::OWNER) => {
                request.owner = String::from(item.as_str().map_err(Error::Parcel)?);
            }
            (Kind::String, field::SINK) => {
                request.sink = Some(String::from(item.as_str().map_err(Error::Parcel)?));
            }
            (Kind::Array, field::MIMES) => {
                let mut nested = item.nested(0).map_err(Error::Parcel)?;
                while let Some(entry) = nested.next().map_err(Error::Parcel)? {
                    if entry.kind == Kind::String {
                        request
                            .mimes
                            .push(String::from(entry.as_str().map_err(Error::Parcel)?));
                    }
                }
            }
            (Kind::Array, field::DATA) => {
                let mut nested = item.nested(0).map_err(Error::Parcel)?;
                while let Some(entry) = nested.next().map_err(Error::Parcel)? {
                    if entry.kind != Kind::Struct {
                        continue;
                    }
                    let mut record = entry.nested(0).map_err(Error::Parcel)?;
                    let mut mime = String::new();
                    let mut bytes = Vec::new();
                    while let Some(part) = record.next().map_err(Error::Parcel)? {
                        match (part.kind, part.id) {
                            (Kind::String, field::MIME) => {
                                mime = String::from(part.as_str().map_err(Error::Parcel)?);
                            }
                            (Kind::Bytes, field::BYTES) => {
                                bytes = part.as_bytes().to_vec();
                            }
                            _ => {}
                        }
                    }
                    request.data.push((mime, bytes));
                }
            }
            _ => {}
        }
    }
    Ok(request)
}

/// Decode a `Request` (or `Serialize`) into `(token, mime)`.
pub fn decode_request(parcel: &Parcel) -> Result<(u64, String)> {
    let mut token = 0u64;
    let mut mime = String::new();
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(item) = decoder.next().map_err(Error::Parcel)? {
        match (item.kind, item.id) {
            (Kind::U64, field::TOKEN) => token = item.as_u64().map_err(Error::Parcel)?,
            (Kind::String, field::MIME) => {
                mime = String::from(item.as_str().map_err(Error::Parcel)?);
            }
            _ => {}
        }
    }
    Ok((token, mime))
}

/// Decode a `Serialize` into `(token, mime)`.
pub fn decode_serialize(parcel: &Parcel) -> Result<(u64, String)> {
    decode_request(parcel)
}

/// Decode a `Request`/`Serialize` reply's payload bytes.
pub fn decode_bytes(parcel: &Parcel) -> Result<Vec<u8>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(item) = decoder.next().map_err(Error::Parcel)? {
        if item.kind == Kind::Bytes && item.id == field::BYTES {
            return Ok(item.as_bytes().to_vec());
        }
    }
    Err(Error::Errno(-errno::EINVAL))
}

/// Decode an `Offer` reply's token.
pub fn decode_token(parcel: &Parcel) -> Result<u64> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(item) = decoder.next().map_err(Error::Parcel)? {
        if item.kind == Kind::U64 && item.id == field::TOKEN {
            return item.as_u64().map_err(Error::Parcel);
        }
    }
    Err(Error::Errno(-errno::EINVAL))
}

/// Decode a `Current` reply into the live offer's metadata.
pub fn decode_current(parcel: &Parcel) -> Result<Option<OfferInfo>> {
    let mut found = false;
    let mut info = OfferInfo::default();
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(item) = decoder.next().map_err(Error::Parcel)? {
        match (item.kind, item.id) {
            (Kind::U64, field::FOUND) => found = item.as_u64().map_err(Error::Parcel)? != 0,
            (Kind::Struct, field::OFFER) => info = decode_info_record(item)?,
            _ => {}
        }
    }
    Ok(found.then_some(info))
}

/// Decode a changed-event payload (the bytes the topic broker carries)
/// into the offer metadata.
pub fn decode_changed(event: &router::Event) -> Result<OfferInfo> {
    let parcel = Parcel::decode(&event.payload).map_err(Error::Parcel)?;
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(item) = decoder.next().map_err(Error::Parcel)? {
        if item.kind == Kind::Struct && item.id == field::OFFER {
            return decode_info_record(item);
        }
    }
    Err(Error::Errno(-errno::EINVAL))
}

/// Decode one `OFFER` metadata record.
fn decode_info_record(record: Field<'_>) -> Result<OfferInfo> {
    let mut info = OfferInfo::default();
    let mut nested = record.nested(0).map_err(Error::Parcel)?;
    while let Some(item) = nested.next().map_err(Error::Parcel)? {
        match (item.kind, item.id) {
            (Kind::U64, field::TOKEN) => info.token = item.as_u64().map_err(Error::Parcel)?,
            (Kind::String, field::OWNER) => {
                info.owner = String::from(item.as_str().map_err(Error::Parcel)?);
            }
            (Kind::U64, field::SESSION) => {
                info.session = item.as_u64().map_err(Error::Parcel)?;
            }
            (Kind::Bool, field::LAZY) => info.lazy = item.as_bool().map_err(Error::Parcel)?,
            (Kind::U64, field::TICK) => info.tick = item.as_u64().map_err(Error::Parcel)?,
            (Kind::Array, field::MIMES) => {
                let mut mimes = item.nested(0).map_err(Error::Parcel)?;
                while let Some(entry) = mimes.next().map_err(Error::Parcel)? {
                    if entry.kind == Kind::String {
                        info.mimes
                            .push(String::from(entry.as_str().map_err(Error::Parcel)?));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(info)
}
