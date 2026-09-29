//! Round-trip tests for the generated `os.lazy.confd.v1` stubs (issue #260).
//!
//! These cover the two codec shapes echo.midl never exercised — `Option` and
//! `Array` — because a `midlc` regression had written their element into the
//! outer encoder, producing a body the decoder could not read back.

use messenger_generated::os_lazy_confd_v1::*;

fn round_trip_value(kind: u32, value: Value) -> Value {
    let body = encode_value(&value).unwrap();
    let decoded = decode_value(&body).unwrap();
    assert_eq!(decoded, value);
    assert_eq!(decoded.kind, kind);
    decoded
}

#[test]
fn value_option_kinds_roundtrip() {
    round_trip_value(
        0,
        Value {
            kind: 0,
            bool_value: Some(true),
            ..Value::default()
        },
    );
    round_trip_value(
        1,
        Value {
            kind: 1,
            i64_value: Some(-42),
            ..Value::default()
        },
    );
    round_trip_value(
        2,
        Value {
            kind: 2,
            u64_value: Some(1500),
            ..Value::default()
        },
    );
    round_trip_value(
        3,
        Value {
            kind: 3,
            str_value: Some("dark".into()),
            ..Value::default()
        },
    );
    round_trip_value(
        4,
        Value {
            kind: 4,
            bytes_value: Some(vec![0, 1, 2, 255]),
            ..Value::default()
        },
    );
}

#[test]
fn get_reply_distinguishes_absent() {
    let absent = GetReply { value: None };
    let body = encode_get_reply(&absent).unwrap();
    assert_eq!(decode_get_reply(&body).unwrap(), absent);

    let present = GetReply {
        value: Some(Value {
            kind: 3,
            str_value: Some("eth0".into()),
            ..Value::default()
        }),
    };
    let body = encode_get_reply(&present).unwrap();
    assert_eq!(decode_get_reply(&body).unwrap(), present);
}

#[test]
fn set_args_carry_path_and_value() {
    let args = SetArgs {
        path: "sys/net/mtu".into(),
        value: Value {
            kind: 2,
            u64_value: Some(1500),
            ..Value::default()
        },
    };
    let body = encode_set_args(&args).unwrap();
    assert_eq!(decode_set_args(&body).unwrap(), args);
}

#[test]
fn list_reply_carries_an_array() {
    let reply = ListReply {
        paths: vec!["sys/a".into(), "sys/a/b".into()],
    };
    let body = encode_list_reply(&reply).unwrap();
    assert_eq!(decode_list_reply(&body).unwrap(), reply);

    let empty = ListReply::default();
    let body = encode_list_reply(&empty).unwrap();
    assert_eq!(decode_list_reply(&body).unwrap(), empty);
}

#[test]
fn method_ids_are_stable() {
    // Golden values: changing these is a breaking change and must bump `.vN`.
    assert_eq!(METHOD_GET, 915881719);
    assert_eq!(METHOD_SET, 682729123);
    assert_eq!(METHOD_DELETE, 1469573738);
    assert_eq!(METHOD_LIST, 220805025);
}
