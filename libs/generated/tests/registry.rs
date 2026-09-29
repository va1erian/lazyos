//! Round-trip tests for the generated `os.lazy.messenger.registry.v1` stubs
//! (issue #300).

use libmessenger::{Encoder, Parcel};
use messenger_generated::os_lazy_messenger_registry_v1::*;

fn entry(name: &str, interfaces: Vec<u64>, lease: u64) -> Entry {
    Entry {
        name: name.into(),
        object: 0xdead_beef_0000_0001,
        owner: 7,
        interfaces,
        lease_remaining: lease,
    }
}

#[test]
fn register_args_roundtrip_with_interface_arrays() {
    for interfaces in [vec![], vec![INTERFACE_ID], (0..16).collect::<Vec<u64>>()] {
        let args = RegisterArgs {
            name: "os.lazy.echo".into(),
            endpoint: Some(42),
            interfaces,
            lease_ticks: u64::MAX,
        };
        let body = encode_register_args(&args).unwrap();
        assert_eq!(decode_register_args(&body).unwrap(), args);
    }
}

#[test]
fn name_only_methods_roundtrip() {
    let resolve = ResolveArgs {
        name: "os.lazy.keyd".into(),
    };
    let body = encode_resolve_args(&resolve).unwrap();
    assert_eq!(decode_resolve_args(&body).unwrap(), resolve);

    let unregister = UnregisterArgs {
        name: "os.lazy.keyd".into(),
    };
    let body = encode_unregister_args(&unregister).unwrap();
    assert_eq!(decode_unregister_args(&body).unwrap(), unregister);

    let reply = ResolveReply { handle: 9 };
    let body = encode_resolve_reply(&reply).unwrap();
    assert_eq!(decode_resolve_reply(&body).unwrap(), reply);
}

#[test]
fn list_reply_empty_and_large() {
    let empty = ListReply::default();
    let body = encode_list_reply(&empty).unwrap();
    assert_eq!(decode_list_reply(&body).unwrap(), empty);

    // The kernel table holds at most 64 names of 128 bytes with 16 interfaces.
    let entries: Vec<Entry> = (0..64)
        .map(|index| {
            let name = format!("{index:03}.{}", "n".repeat(120));
            entry(&name, (0..16).map(|i| i as u64).collect(), index as u64)
        })
        .collect();
    let big = ListReply { entries };
    let body = encode_list_reply(&big).unwrap();
    assert_eq!(decode_list_reply(&body).unwrap(), big);
}

#[test]
fn entry_roundtrip_including_empty_name_and_permanent_lease() {
    for value in [entry("", vec![], 0), entry("os.lazy.x", vec![1, 2, 3], 500)] {
        let body = encode_entry(&value).unwrap();
        assert_eq!(decode_entry(&body).unwrap(), value);
    }
}

#[test]
fn a_full_parcel_survives_the_codec() {
    let reply = ListReply {
        entries: vec![entry("os.lazy.a", vec![5], 0)],
    };
    let parcel = Parcel {
        header: libmessenger::Header {
            version: libmessenger::VERSION,
            flags: 0,
            interface_id: INTERFACE_ID,
            method: METHOD_LIST,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: encode_list_reply(&reply).unwrap(),
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).unwrap();
    let back = Parcel::decode(&bytes).unwrap();
    assert_eq!(decode_list_reply(&back.body).unwrap(), reply);
}

#[test]
fn truncated_body_is_rejected() {
    let body = encode_register_args(&RegisterArgs {
        name: "os.lazy.echo".into(),
        endpoint: Some(1),
        interfaces: vec![1, 2],
        lease_ticks: 3,
    })
    .unwrap();
    for cut in [1, body.len() / 2, body.len() - 1] {
        assert!(decode_register_args(&body[..cut]).is_err(), "cut at {cut}");
    }
    let list = encode_list_reply(&ListReply {
        entries: vec![entry("os.lazy.a", vec![1], 2)],
    })
    .unwrap();
    assert!(decode_list_reply(&list[..list.len() - 1]).is_err());
}

#[test]
fn decoders_ignore_unknown_fields_such_as_the_error_field() {
    // Field 15 is the hand-written daemon error field; ids past the generated
    // range must never break a decoder.
    let mut named = Encoder::new();
    named.string(1, "os.lazy.echo").unwrap();
    named.u64(99, 1).unwrap();
    named.error(15, 3, "denied").unwrap();
    let args = decode_resolve_args(&named.finish()).unwrap();
    assert_eq!(args.name, "os.lazy.echo");

    let mut failure = Encoder::new();
    failure.u64(99, 1).unwrap();
    failure.error(15, 3, "denied").unwrap();
    let failure = failure.finish();
    assert_eq!(decode_resolve_reply(&failure).unwrap().handle, 0);
    assert!(decode_list_reply(&failure).unwrap().entries.is_empty());
}

#[test]
fn method_ids_are_stable() {
    // Golden values: changing these is a breaking change and must bump `.vN`.
    assert_eq!(METHOD_REGISTER, 658098656);
    assert_eq!(METHOD_RESOLVE, 1645633795);
    assert_eq!(METHOD_UNREGISTER, 1480320227);
    assert_eq!(METHOD_LIST, 220805025);
    assert_eq!(INTERFACE_ID, 0x51d501afec09806c);
}

#[test]
fn absent_endpoint_is_distinguishable_from_handle_zero() {
    let absent = encode_register_args(&RegisterArgs {
        name: "os.lazy.echo".into(),
        endpoint: None,
        interfaces: vec![],
        lease_ticks: 0,
    })
    .unwrap();
    assert_eq!(decode_register_args(&absent).unwrap().endpoint, None);
    let zero = encode_register_args(&RegisterArgs {
        name: "os.lazy.echo".into(),
        endpoint: Some(0),
        interfaces: vec![],
        lease_ticks: 0,
    })
    .unwrap();
    assert_eq!(decode_register_args(&zero).unwrap().endpoint, Some(0));
}
