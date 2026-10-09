//! Round-trip tests for the generated `os.lazy.echo.v1` stubs (issue #90).
//!
//! These run on the host: the generated code only depends on the `libmessenger`
//! codec, so a generated client/server pair can be tested without QEMU.

use libmessenger::Buffer;
use messenger_generated::os_lazy_echo_v1::*;

#[test]
fn interface_and_method_ids_are_stable() {
    // Golden values: changing these is a breaking change and must bump `.vN`.
    assert_eq!(METHOD_ECHO, 998075300);
    assert_eq!(METHOD_PING, 2142761129);
    assert_eq!(METHOD_NOTIFY, 314575196);
}

#[test]
fn echo_args_roundtrip() {
    let args = EchoArgs {
        text: "hello messenger".into(),
        count: 15,
    };
    let body = encode_echo_args(&args).unwrap();
    assert_eq!(decode_echo_args(&body).unwrap(), args);
}

#[test]
fn echo_reply_roundtrip() {
    let reply = EchoReply {
        reply: "hello messenger".into(),
    };
    let body = encode_echo_reply(&reply).unwrap();
    assert_eq!(decode_echo_reply(&body).unwrap(), reply);
}

#[test]
fn ping_reply_roundtrip() {
    let reply = PingReply { alive: true };
    let body = encode_ping_reply(&reply).unwrap();
    assert_eq!(decode_ping_reply(&body).unwrap(), reply);
}

#[test]
fn notify_carries_a_struct() {
    let args = NotifyArgs {
        event: Event {
            topic: "system/events/network".into(),
            at: 42,
        },
    };
    let body = encode_notify_args(&args).unwrap();
    assert_eq!(decode_notify_args(&body).unwrap(), args);
}

#[test]
fn unknown_fields_are_ignored() {
    // A newer sender may append a field; an older reader must ignore it.
    let args = EchoArgs {
        text: "x".into(),
        count: 1,
    };
    let mut body = encode_echo_args(&args).unwrap();
    let mut extra = libmessenger::Encoder::new();
    extra.u32(99, 7).unwrap();
    body.extend_from_slice(extra.as_bytes());
    assert_eq!(decode_echo_args(&body).unwrap(), args);
}

#[test]
fn buffer_fields_are_the_codec_type() {
    // Not part of echo.midl today; a `Buffer` parameter is the codec's own
    // type, usable from generated code without a re-export.
    let buffer = Buffer::whole(3, 4096);
    assert_eq!((buffer.handle, buffer.offset, buffer.len), (3, 0, 4096));
}
