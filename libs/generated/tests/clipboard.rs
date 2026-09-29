//! Round-trip tests for the generated `os.lazy.clipboard.v1` stubs
//! (issue #285). They cover the shapes the clipboard wire relies on: an
//! `Option<String>` sink, an `Array` of structs, an `Option` of a struct
//! (`Current`), and byte payloads.

use messenger_generated::os_lazy_clipboard_v1::*;

fn meta() -> OfferMeta {
    OfferMeta {
        token: 7,
        owner: "clipcopy".into(),
        session: 3,
        mimes: vec!["text/plain".into(), "text/uri-list".into()],
        lazy: true,
        tick: 12_345,
    }
}

#[test]
fn eager_offer_carries_payload_records() {
    let args = OfferArgs {
        owner: "editor".into(),
        sink: None,
        mimes: vec!["text/plain".into(), "image/x-raw".into()],
        data: vec![
            Payload {
                mime: "text/plain".into(),
                bytes: b"hello".to_vec(),
            },
            Payload {
                mime: "image/x-raw".into(),
                bytes: vec![0, 1, 2, 255],
            },
        ],
    };
    let decoded = decode_offer_args(&encode_offer_args(&args).unwrap()).unwrap();
    assert_eq!(decoded, args);
    assert_eq!(decoded.sink, None);
}

#[test]
fn lazy_offer_carries_sink_and_no_data() {
    let args = OfferArgs {
        owner: "clipcopy".into(),
        sink: Some("os.lazy.clipboard.owner.clipcopy".into()),
        mimes: vec!["text/plain".into()],
        data: Vec::new(),
    };
    let decoded = decode_offer_args(&encode_offer_args(&args).unwrap()).unwrap();
    assert_eq!(decoded, args);
}

#[test]
fn empty_payload_bytes_survive() {
    let args = OfferArgs {
        owner: "o".into(),
        sink: None,
        mimes: vec!["text/plain".into()],
        data: vec![Payload {
            mime: "text/plain".into(),
            bytes: Vec::new(),
        }],
    };
    let decoded = decode_offer_args(&encode_offer_args(&args).unwrap()).unwrap();
    assert_eq!(decoded, args);
}

#[test]
fn token_request_and_serialize_roundtrip() {
    let reply = OfferReply { token: u64::MAX };
    assert_eq!(
        decode_offer_reply(&encode_offer_reply(&reply).unwrap()).unwrap(),
        reply
    );
    let request = RequestArgs {
        token: 0,
        mime: "text/plain".into(),
    };
    assert_eq!(
        decode_request_args(&encode_request_args(&request).unwrap()).unwrap(),
        request
    );
    let serialize = SerializeArgs {
        token: 9,
        mime: "text/uri-list".into(),
    };
    assert_eq!(
        decode_serialize_args(&encode_serialize_args(&serialize).unwrap()).unwrap(),
        serialize
    );
    let bytes = SerializeReply {
        bytes: b"payload".to_vec(),
    };
    assert_eq!(
        decode_serialize_reply(&encode_serialize_reply(&bytes).unwrap()).unwrap(),
        bytes
    );
}

#[test]
fn request_reply_carries_the_buffer_shape() {
    let reply = RequestReply {
        token: 4,
        mime: "text/plain".into(),
        lazy: true,
        bytes: vec![b'x'; 8 * 1024],
    };
    let decoded = decode_request_reply(&encode_request_reply(&reply).unwrap()).unwrap();
    assert_eq!(decoded, reply);
}

#[test]
fn current_reply_distinguishes_no_offer() {
    let none = CurrentReply { offer: None };
    let decoded = decode_current_reply(&encode_current_reply(&none).unwrap()).unwrap();
    assert_eq!(decoded, none);

    let some = CurrentReply {
        offer: Some(meta()),
    };
    let decoded = decode_current_reply(&encode_current_reply(&some).unwrap()).unwrap();
    assert_eq!(decoded, some);
}

#[test]
fn offer_meta_roundtrips_alone() {
    let decoded = decode_offer_meta(&encode_offer_meta(&meta()).unwrap()).unwrap();
    assert_eq!(decoded, meta());
}

#[test]
fn ids_are_stable() {
    // Golden values: changing these is a breaking change and must bump `.vN`.
    assert_eq!(INTERFACE_ID, 0x5a8da8f22670b758);
    assert_eq!(METHOD_OFFER, 1313375869);
    assert_eq!(METHOD_REQUEST, 38093138);
    assert_eq!(METHOD_SERIALIZE, 1116160801);
    assert_eq!(METHOD_PING, 2142761129);
    assert_eq!(METHOD_CURRENT, 869319546);
}
