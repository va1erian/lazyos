//! Round-trip tests for the generated `os.lazy.mimed.v1` stubs (issue #285).

use messenger_generated::os_lazy_mimed_v1::*;

#[test]
fn guess_roundtrip() {
    let args = GuessArgs {
        path: "/home/a/notes.txt".into(),
    };
    assert_eq!(
        decode_guess_args(&encode_guess_args(&args).unwrap()).unwrap(),
        args
    );
    let reply = GuessReply {
        mime: "text/plain".into(),
    };
    assert_eq!(
        decode_guess_reply(&encode_guess_reply(&reply).unwrap()).unwrap(),
        reply
    );
}

#[test]
fn lookup_option_present_and_absent() {
    let args = LookupArgs {
        mime: "text/plain".into(),
        verb: "open".into(),
    };
    assert_eq!(
        decode_lookup_args(&encode_lookup_args(&args).unwrap()).unwrap(),
        args
    );
    for app in [Some(String::from("editor")), None] {
        let reply = LookupReply { app };
        let back = decode_lookup_reply(&encode_lookup_reply(&reply).unwrap()).unwrap();
        assert_eq!(back, reply);
    }
}

#[test]
fn verbs_array_roundtrip_keeps_order_and_empty() {
    for verbs in [
        vec![],
        vec!["open".to_string(), "edit".into(), "reveal".into()],
    ] {
        let reply = VerbsReply { verbs };
        let back = decode_verbs_reply(&encode_verbs_reply(&reply).unwrap()).unwrap();
        assert_eq!(back, reply);
    }
}

#[test]
fn open_reply_carries_every_field() {
    let reply = OpenReply {
        app: "editor".into(),
        mime: "text/plain".into(),
        topic: name_system_events_open("editor").unwrap(),
        published: true,
        launched: false,
    };
    assert_eq!(
        decode_open_reply(&encode_open_reply(&reply).unwrap()).unwrap(),
        reply
    );
    let args = OpenArgs {
        path: "/a.txt".into(),
        verb: "open".into(),
    };
    assert_eq!(
        decode_open_args(&encode_open_args(&args).unwrap()).unwrap(),
        args
    );
}

#[test]
fn register_args_roundtrip() {
    let args = RegisterArgs {
        mime: "image/png".into(),
        app: "viewer".into(),
        verb: "open".into(),
    };
    assert_eq!(
        decode_register_args(&encode_register_args(&args).unwrap()).unwrap(),
        args
    );
}

#[test]
fn decoders_ignore_unknown_fields_such_as_the_error_field() {
    // A service failure carries only the structured error field (id 15);
    // a decoder must yield defaults rather than fail on it.
    let mut body = libmessenger::Encoder::new();
    body.error(messenger_generated::errors::ERROR_FIELD, 22, "bad")
        .unwrap();
    let decoded = decode_guess_reply(&body.finish()).unwrap();
    assert_eq!(decoded, GuessReply::default());
}

#[test]
fn open_event_roundtrips_and_builds_a_concrete_topic() {
    let event = OpenEvent {
        path: "NOTES.TXT".into(),
        mime: "text/plain".into(),
        verb: "open".into(),
    };
    assert_eq!(
        decode_system_events_open(&encode_system_events_open(&event).unwrap()).unwrap(),
        event
    );
    assert_eq!(
        name_system_events_open("editor").unwrap(),
        "system/events/open/editor"
    );
    for bad in ["", "+", "#", "a/b", "a#b"] {
        assert!(name_system_events_open(bad).is_err(), "accepted {bad:?}");
    }
}

#[test]
fn malformed_open_event_payloads_are_rejected_not_panicked() {
    let body = encode_open_event(&OpenEvent {
        path: "A.TXT".into(),
        mime: "text/plain".into(),
        verb: "open".into(),
    })
    .unwrap();
    assert!(decode_open_event(&body[..body.len() - 2]).is_err());
    assert!(decode_open_event(&[0xff, 0xff, 0xff, 0xff]).is_err());
    assert!(decode_open_event(&[0x01]).is_err());
}
