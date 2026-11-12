//! Parcel helpers for the clipboard protocol.
//!
//! The wire shapes are the `midlc`-generated `os.lazy.clipboard.v1` stubs
//! ([`super::wire`]); this module frames them in parcels on the right scope
//! interface and converts to the module's plain Rust types. The structured
//! error rides in the standard error field every service shares
//! (`messenger_generated::errors`).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::Parcel;

use super::super::{Error, Result};
use super::{parcel, wire, OfferInfo, OWNER_INTERFACE};

/// `Serialize(token, mime)`: the service calls this on a lazy offer's owner
/// endpoint when a paste happens.
pub fn serialize_request(token: u64, mime: &str) -> Result<Parcel> {
    let args = wire::SerializeArgs {
        token,
        mime: String::from(mime),
    };
    let body = wire::encode_serialize_args(&args).map_err(Error::Parcel)?;
    Ok(parcel(OWNER_INTERFACE, wire::METHOD_SERIALIZE, body))
}

/// The owner's `Serialize` answer.
pub fn serialize_reply(bytes: &[u8]) -> Result<Parcel> {
    let reply = wire::SerializeReply {
        bytes: bytes.to_vec(),
    };
    let body = wire::encode_serialize_reply(&reply).map_err(Error::Parcel)?;
    Ok(parcel(OWNER_INTERFACE, wire::METHOD_SERIALIZE, body))
}

/// The generated metadata record for `info`.
pub fn to_wire_meta(info: &OfferInfo) -> wire::OfferMeta {
    wire::OfferMeta {
        token: info.token,
        owner: info.owner.clone(),
        session: info.session,
        mimes: info.mimes.clone(),
        lazy: info.lazy,
        tick: info.tick,
    }
}

/// The plain-Rust view of a generated metadata record.
pub fn from_wire_meta(meta: wire::OfferMeta) -> OfferInfo {
    OfferInfo {
        token: meta.token,
        owner: meta.owner,
        session: meta.session,
        mimes: meta.mimes,
        lazy: meta.lazy,
        tick: meta.tick,
    }
}

/// Decode a `Serialize` into `(token, mime)`.
pub fn decode_serialize(parcel: &Parcel) -> Result<(u64, String)> {
    let args = wire::decode_serialize_args(&parcel.body).map_err(Error::Parcel)?;
    Ok((args.token, args.mime))
}

/// Decode a `Serialize` reply's payload bytes.
pub fn decode_serialized(parcel: &Parcel) -> Result<Vec<u8>> {
    let reply = wire::decode_serialize_reply(&parcel.body).map_err(Error::Parcel)?;
    Ok(reply.bytes)
}

/// Render a decoded `PasteEvent` as the one-line text `logd` records, so a
/// consumer of the typed `system/events/clipboard/paste` topic displays the
/// same fields the hand-encoded payload used to carry.
pub fn paste_event_text(event: &wire::PasteEvent) -> String {
    format!(
        "paste #{} uid={} session={} mime={} app={} bytes={} lazy={}",
        event.seq, event.uid, event.session, event.mime, event.app, event.bytes, event.lazy
    )
}

/// Render a decoded `ClipboardDenial` as the one-line text `logd` records for
/// the `system/events/security/clipboard` topic.
pub fn denial_event_text(event: &wire::ClipboardDenial) -> String {
    format!(
        "deny #{} uid={} session={} mime={} app={} token={}",
        event.seq, event.uid, event.session, event.mime, event.app, event.token
    )
}
