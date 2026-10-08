//! Round-trip tests for the I3 additions to `idl/input.midl`: grabs, the
//! escape chord, `Ping` and the key-state page (`docs/input-plan.md`).

use messenger_generated::os_lazy_input_shell_v1 as shell;
use messenger_generated::os_lazy_input_v1 as input;

#[test]
fn new_method_ids_are_appended_and_pinned() {
    assert_eq!(
        [
            input::METHOD_REQUESTGRANT,
            input::METHOD_RELEASEGRANT,
            input::METHOD_PING,
            input::METHOD_ATTACHKEYSTATE,
            input::METHOD_GRANTCHANGED,
        ],
        [4, 5, 6, 7, 15]
    );
    assert_eq!(shell::METHOD_GRABCHANGED, 26);
    assert_eq!([input::GRANT_KIND_NONE, input::GRANT_KIND_KEYBOARD], [0, 1]);
    assert_eq!(
        [
            input::GRANT_REASON_APPROVED,
            input::GRANT_REASON_DENIED,
            input::GRANT_REASON_RELEASED,
            input::GRANT_REASON_FOCUS_LOST,
            input::GRANT_REASON_ESCAPED,
            input::GRANT_REASON_CLOSED,
        ],
        [0, 1, 2, 3, 4, 5]
    );
}

#[test]
fn grant_records_roundtrip() {
    let body = input::encode_request_grant_args(&input::RequestGrantArgs {
        session: 9,
        kind: input::GRANT_KIND_KEYBOARD,
    })
    .unwrap();
    let args = input::decode_request_grant_args(&body).unwrap();
    assert_eq!((args.session, args.kind), (9, 1));
    for active in [false, true] {
        let body = input::encode_grant_changed_args(&input::GrantChangedArgs {
            kind: 1,
            active,
            reason: input::GRANT_REASON_ESCAPED,
        })
        .unwrap();
        let args = input::decode_grant_changed_args(&body).unwrap();
        assert_eq!((args.active, args.reason), (active, 4));
    }
    let body = shell::encode_grant_requested_args(&shell::GrantRequestedArgs {
        session: 3,
        kind: 1,
        surface: u64::MAX,
    })
    .unwrap();
    let args = shell::decode_grant_requested_args(&body).unwrap();
    assert_eq!((args.session, args.kind, args.surface), (3, 1, u64::MAX));
    for surface in [None, Some(0), Some(77)] {
        let body = shell::encode_grab_changed_args(&shell::GrabChangedArgs { surface }).unwrap();
        assert_eq!(
            shell::decode_grab_changed_args(&body).unwrap().surface,
            surface
        );
    }
    let body = shell::encode_approve_grant_args(&shell::ApproveGrantArgs {
        session: 5,
        allow: true,
    })
    .unwrap();
    let args = shell::decode_approve_grant_args(&body).unwrap();
    assert!(args.allow && args.session == 5);
}

#[test]
fn ping_and_key_state_page_wire() {
    let body = input::encode_ping_reply(&input::PingReply {
        token: 0xDEAD,
        seq: 42,
    })
    .unwrap();
    let reply = input::decode_ping_reply(&body).unwrap();
    assert_eq!((reply.token, reply.seq), (0xDEAD, 42));
    // The page travels as exactly one buffer object, the request's `state`.
    assert_eq!(
        input::ATTACH_KEY_STATE_OBJECTS,
        &[libmessenger::ObjectKind::Buffer]
    );
    assert_eq!(
        messenger_generated::declared_objects(input::INTERFACE_ID, input::METHOD_ATTACHKEYSTATE),
        input::ATTACH_KEY_STATE_OBJECTS
    );
    let args = input::AttachKeyStateArgs {
        session: 9,
        state: libmessenger::Buffer::whole(4, 4096),
    };
    let (body, objects) = input::encode_attach_key_state_args(&args).unwrap();
    assert_eq!(objects, vec![libmessenger::Object::Buffer(4)]);
    // The receiver decodes against the installed list: the handle is its own.
    let installed = [libmessenger::Object::Buffer(17)];
    let back = input::decode_attach_key_state_args(&body, &installed).unwrap();
    assert_eq!(back.session, 9);
    assert_eq!(back.state, libmessenger::Buffer::whole(17, 4096));
    // A list that does not match the declaration is refused.
    assert!(input::decode_attach_key_state_args(&body, &[]).is_err());
    assert!(input::decode_attach_key_state_args(&body, &[libmessenger::Object::Channel(17)]).is_err());
}
