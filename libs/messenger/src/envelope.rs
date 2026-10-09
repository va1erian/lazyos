//! The central broker's payload envelope.
//!
//! `messengerd` stores and delivers each topic payload as an encoded parcel.
//! The platform's publishers (`user::central::Bus`, the Rhai `msg` module)
//! put the raw payload bytes (a generated topic codec's output, or text) in a
//! one-field parcel stamped [`INTERFACE`]: UTF-8 in field 1, anything else in
//! field 2. Every reader of a central-broker event unwraps it with [`unwrap`],
//! so the envelope has exactly one definition (this one).

use alloc::vec::Vec;

use crate::{Decoder, Encoder, Error, Header, Kind, Parcel, VERSION};

/// The envelope's interface marker. The broker stores payloads verbatim and
/// never delivers this parcel as a request, so it is an opaque tag.
pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.cntrl");
/// The field of a UTF-8 payload.
pub const FIELD_TEXT: u16 = 1;
/// The field of any other payload.
pub const FIELD_BYTES: u16 = 2;

/// Wrap `payload` in the envelope: text in [`FIELD_TEXT`], anything else in
/// [`FIELD_BYTES`]. Returns the encoded parcel the broker's `Publish` carries.
pub fn wrap(payload: &[u8]) -> Result<Vec<u8>, Error> {
    let mut body = Encoder::new();
    match core::str::from_utf8(payload) {
        Ok(text) => body.string(FIELD_TEXT, text)?,
        Err(_) => body.bytes(FIELD_BYTES, payload)?,
    }
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: INTERFACE,
            method: 1,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        objects: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes)?;
    Ok(bytes)
}

/// The payload inside an envelope. A parcel that is not one (another
/// publisher's own parcel, or bytes that are no parcel at all) is handed back
/// unchanged, so a reader never misreads an unrelated field 1 as envelope text.
pub fn unwrap(event_payload: &[u8]) -> Vec<u8> {
    let Ok(parcel) = Parcel::decode(event_payload) else {
        return event_payload.to_vec();
    };
    if parcel.header.interface_id != INTERFACE {
        return event_payload.to_vec();
    }
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        match (field.kind, field.id) {
            (Kind::String, FIELD_TEXT) | (Kind::Bytes, FIELD_BYTES) => {
                return field.as_bytes().to_vec()
            }
            _ => {}
        }
    }
    event_payload.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_binary_payloads_round_trip() {
        for payload in [&b"state=running"[..], &[0xff, 0x00, 0x12][..], &[][..]] {
            let wrapped = wrap(payload).expect("wraps");
            assert_ne!(wrapped, payload);
            assert_eq!(unwrap(&wrapped), payload);
        }
    }

    #[test]
    fn a_foreign_parcel_or_garbage_passes_through() {
        let mut foreign = Vec::new();
        let mut body = Encoder::new();
        body.string(FIELD_TEXT, "not an envelope").expect("encodes");
        Parcel {
            header: Header {
                interface_id: 7,
                ..Header::default()
            },
            body: body.finish(),
            objects: Vec::new(),
        }
        .encode(&mut foreign)
        .expect("encodes");
        assert_eq!(unwrap(&foreign), foreign);
        assert_eq!(unwrap(b"\x01\x02"), b"\x01\x02");
    }
}
