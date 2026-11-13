//! The generated typed clients (`docs/client-api-plan.md` 3.2) driven through
//! `libmessenger`'s scripted `Mock` transport: what a client puts on the wire
//! (header id, deadline, body, objects) and how it reads the reply and the
//! errors back. Every request is decoded again with the generated
//! `decode_*_args`, so the client and the codec cannot drift apart.

use libmessenger::client::{
    CallError, Connection, Deadline, Errno, Mock, Recorded, Retry, ERROR_FIELD,
};
use libmessenger::{flags, Buffer, Encoder, Object, Parcel};
use messenger_generated::{
    os_lazy_clipboard_v1 as clipboard, os_lazy_confd_v1 as confd, os_lazy_keyd_v1 as keyd,
    os_lazy_net_nic_v1 as nic, os_lazy_notify_v1 as notify, os_lazy_timed_v1 as timed,
};

fn reply(body: Vec<u8>) -> Parcel {
    Parcel {
        body,
        ..Parcel::default()
    }
}

fn refusal(code: u32, message: &str) -> Parcel {
    let mut body = Encoder::new();
    body.error(ERROR_FIELD, code, message).unwrap();
    reply(body.finish())
}

fn mock(name: &str) -> Mock {
    let mock = Mock::new();
    mock.register(name);
    mock
}

fn only_call(client_mock: &Mock) -> Recorded {
    let mut calls = client_mock.calls();
    assert_eq!(calls.len(), 1, "exactly one request: {calls:?}");
    calls.remove(0)
}

fn text(value: &str) -> confd::Value {
    confd::Value {
        kind: confd::KIND_STR,
        str_value: Some(value.into()),
        ..confd::Value::default()
    }
}

#[test]
fn connect_resolves_the_declared_name_within_the_transports_bound() {
    let client = confd::Client::connect(mock(confd::SERVICE_NAME)).unwrap();
    let resolves = client.connection().transport().resolves();
    assert_eq!(
        resolves,
        vec![(confd::SERVICE_NAME.to_string(), Deadline::secs(10))]
    );
}

#[test]
fn try_connect_makes_one_attempt_and_reports_a_missing_service() {
    let found = confd::Client::try_connect(mock(confd::SERVICE_NAME)).unwrap();
    assert_eq!(
        found.connection().transport().resolves(),
        vec![(confd::SERVICE_NAME.to_string(), Deadline::Poll)]
    );
    let absent = confd::Client::try_connect(Mock::new());
    assert!(matches!(
        absent,
        Err(CallError::Transport(errno)) if errno == Errno::NOENT
    ));
}

#[test]
fn a_one_field_reply_returns_the_field() {
    let transport = mock(confd::SERVICE_NAME);
    transport.reply(reply(
        confd::encode_get_reply(&confd::GetReply {
            value: Some(text("dark")),
        })
        .unwrap(),
    ));
    let client = confd::Client::connect(transport).unwrap();
    assert_eq!(client.get("sys/ui/theme").unwrap(), Some(text("dark")));

    let request = only_call(client.connection().transport());
    assert_eq!(request.header.interface_id, confd::INTERFACE_ID);
    assert_eq!(request.header.method, confd::METHOD_GET);
    assert_eq!(request.until, Some(confd::METHOD_GET_DEADLINE));
    let args = confd::decode_get_args(&request.body).unwrap();
    assert_eq!(args.path, "sys/ui/theme");
}

#[test]
fn an_absent_value_is_none() {
    let transport = mock(confd::SERVICE_NAME);
    transport.reply(reply(
        confd::encode_get_reply(&confd::GetReply { value: None }).unwrap(),
    ));
    let client = confd::Client::connect(transport).unwrap();
    assert_eq!(client.get("missing").unwrap(), None);
}

#[test]
fn an_empty_reply_is_unit_and_structs_are_passed_by_reference() {
    let client = confd::Client::connect(mock(confd::SERVICE_NAME)).unwrap();
    client.set("sys/ui/theme", &text("light")).unwrap();
    let args = confd::decode_set_args(&only_call(client.connection().transport()).body).unwrap();
    assert_eq!(args.path, "sys/ui/theme");
    assert_eq!(args.value, text("light"));
}

#[test]
fn a_declared_service_error_maps_to_the_enum() {
    let transport = mock(confd::SERVICE_NAME);
    transport.reply(refusal(confd::ServiceError::Denied.code(), "not allowed"));
    let client = confd::Client::connect(transport).unwrap();
    assert_eq!(
        client.get("sys/secret"),
        Err(CallError::Service(confd::ServiceError::Denied))
    );
}

#[test]
fn an_undeclared_remote_code_stays_remote_and_keeps_its_text() {
    let transport = mock(confd::SERVICE_NAME);
    transport.reply(refusal(77, "something else"));
    let client = confd::Client::connect(transport).unwrap();
    match client.get("x") {
        Err(CallError::Remote { code, message, .. }) => {
            assert_eq!(code, 77);
            assert_eq!(message, "something else");
        }
        other => panic!("expected a remote refusal, got {other:?}"),
    }
}

#[test]
fn a_transport_failure_is_reported_as_such() {
    let transport = mock(confd::SERVICE_NAME);
    transport.fail(Errno::TIMEDOUT);
    let client = confd::Client::connect(transport).unwrap();
    assert!(client.get("x").unwrap_err().is_timeout());
}

#[test]
fn a_malformed_reply_is_a_bad_message() {
    let transport = mock(confd::SERVICE_NAME);
    // A field header that claims more bytes than the body holds.
    transport.reply(reply(vec![0x01, 0xff, 0xff, 0xff]));
    let client = confd::Client::connect(transport).unwrap();
    assert_eq!(client.get("x"), Err(CallError::Transport(Errno::BADMSG)));
}

#[test]
fn the_deadline_override_replaces_the_declared_deadline() {
    let client = confd::Client::connect(mock(confd::SERVICE_NAME)).unwrap();
    let _ = client.get_within("a", Deadline::millis(50));
    let _ = client.get("a");
    let calls = client.connection().transport().calls();
    assert_eq!(calls[0].until, Some(Deadline::millis(50)));
    assert_eq!(calls[1].until, Some(confd::METHOD_GET_DEADLINE));
}

#[test]
fn clipboard_methods_travel_on_their_scope_with_their_deadline() {
    let client = clipboard::Client::connect(mock(clipboard::SERVICE_NAME)).unwrap();
    let _ = client.offer("editor", None, &["text/plain".to_string()], &[]);
    let _ = client.request(7, "text/plain");
    let calls = client.connection().transport().calls();

    assert_eq!(calls[0].header.interface_id, clipboard::METHOD_OFFER_SCOPE);
    assert_ne!(calls[0].header.interface_id, clipboard::INTERFACE_ID);
    assert_eq!(calls[0].until, Some(Deadline::millis(500)));
    assert_eq!(calls[0].until, Some(clipboard::METHOD_OFFER_DEADLINE));
    let offer = clipboard::decode_offer_args(&calls[0].body).unwrap();
    assert_eq!(offer.owner, "editor");
    assert_eq!(offer.sink, None);
    assert_eq!(offer.mimes, vec!["text/plain".to_string()]);

    assert_eq!(
        calls[1].header.interface_id,
        clipboard::METHOD_REQUEST_SCOPE
    );
    assert_ne!(
        clipboard::METHOD_REQUEST_SCOPE,
        clipboard::METHOD_OFFER_SCOPE
    );
    let request = clipboard::decode_request_args(&calls[1].body).unwrap();
    assert_eq!((request.token, request.mime.as_str()), (7, "text/plain"));
}

#[test]
fn a_buffer_argument_moves_into_the_objects_list() {
    let transport = mock(clipboard::SERVICE_NAME);
    transport.reply(reply(
        clipboard::encode_offer_buffer_reply(&clipboard::OfferBufferReply { token: 9 }).unwrap(),
    ));
    let client = clipboard::Client::connect(transport).unwrap();
    let data = Buffer::whole(41, 4096);
    assert_eq!(client.offer_buffer("viewer", "image/png", data), Ok(9));

    let sent = only_call(client.connection().transport());
    assert_eq!(sent.objects, vec![Object::Buffer(41)]);
    assert_eq!(sent.until, Some(Deadline::secs(5)));
    assert_eq!(sent.header.interface_id, clipboard::INTERFACE_ID);
    let args = clipboard::decode_offer_buffer_args(&sent.body, &sent.objects).unwrap();
    assert_eq!(args.data, data);
    assert_eq!(args.mime, "image/png");
}

#[test]
fn a_channel_argument_moves_and_a_multi_field_reply_is_a_struct() {
    let transport = mock(notify::SERVICE_NAME);
    let client = notify::Client::connect(transport).unwrap();
    client.listen(55).unwrap();
    let sent = only_call(client.connection().transport());
    assert_eq!(sent.objects, vec![Object::Channel(55)]);
    assert_eq!(sent.until, Some(notify::METHOD_LISTEN_DEADLINE));
    let args = notify::decode_listen_args(&sent.body, &sent.objects).unwrap();
    assert_eq!(args.events, 55);

    let transport = mock(timed::SERVICE_NAME);
    let now = timed::NowReply {
        unix_ms: 1_700_000_000_000,
        tz_offset_s: 3600,
        tz_name: "CET".into(),
        dst: false,
    };
    transport.reply(reply(timed::encode_now_reply(&now).unwrap()));
    let client = timed::Client::connect(transport).unwrap();
    assert_eq!(client.now().unwrap(), now);
}

#[test]
fn a_one_field_string_reply_and_a_string_argument() {
    let transport = mock(timed::SERVICE_NAME);
    transport.reply(reply(
        timed::encode_get_zone_reply(&timed::GetZoneReply { name: "UTC".into() }).unwrap(),
    ));
    let client = timed::Client::connect(transport).unwrap();
    assert_eq!(client.get_zone().unwrap(), "UTC");
    client.set_zone("Europe/Paris").unwrap();
    let calls = client.connection().transport().calls();
    assert_eq!(calls[0].body, Vec::<u8>::new());
    let zone = timed::decode_set_zone_args(&calls[1].body).unwrap();
    assert_eq!(zone.name, "Europe/Paris");
    assert_eq!(calls[1].header.method, timed::METHOD_SETZONE);
}

/// One member of the NIC family: a name built from the prefix and the instance.
const ETH0: &str = "os.lazy.net.nic/eth0";

#[test]
fn a_oneway_method_sends_without_waiting() {
    assert!(ETH0.starts_with(nic::SERVICE_INSTANCE_PREFIX));
    let client = nic::Client::from_connection(Connection::new(mock(ETH0), ETH0));
    client.kick(1).unwrap();

    let sent = only_call(client.connection().transport());
    assert_eq!(sent.until, None, "a one-way send has no deadline");
    assert_eq!(sent.header.flags & flags::ONE_WAY, flags::ONE_WAY);
    assert_eq!(sent.header.interface_id, nic::INTERFACE_ID);
    assert_eq!(sent.header.method, nic::METHOD_KICK);
    assert_eq!(nic::decode_kick_args(&sent.body).unwrap().ring, 1);
}

#[test]
fn a_family_interface_has_no_singleton_name() {
    // `nic::Client::connect` does not exist (a family has no one name); the
    // instance's connection is built by the caller, as above.
    assert!(nic::SERVICE_INSTANCE_PREFIX.ends_with('/'));
}

#[test]
fn a_restarted_service_is_found_again_through_the_client() {
    let transport = mock(confd::SERVICE_NAME);
    let client = confd::Client::connect(transport).unwrap();
    client.get("a").unwrap();
    client.connection().transport().kill(confd::SERVICE_NAME);
    client
        .connection()
        .transport()
        .register(confd::SERVICE_NAME);
    client.get("a").unwrap();
    assert_eq!(client.connection().transport().calls().len(), 3);
}

#[test]
fn the_idl_policy_is_a_constant_and_the_client_obeys_it() {
    assert_eq!(confd::METHOD_GET_RETRY, Retry::Safe);
    assert_eq!(timed::METHOD_SYNCNOW_RETRY, Retry::Never);
    assert_eq!(keyd::METHOD_VERIFY_DEADLINE, Deadline::Never);
    assert_eq!(confd::METHOD_GET_DEADLINE, Deadline::millis(2000));

    // A method that waits for ever says so to the transport.
    let client = keyd::Client::connect(mock(keyd::SERVICE_NAME)).unwrap();
    let _ = client.verify("user", "secret");
    assert_eq!(
        only_call(client.connection().transport()).until,
        Some(Deadline::Never)
    );
}

#[test]
fn a_safe_method_is_resent_after_the_service_died_handling_it() {
    let transport = mock(confd::SERVICE_NAME);
    let client = confd::Client::connect(transport).unwrap();
    client.connection().transport().fail(Errno::PIPE);
    client.get("a").unwrap();
    assert_eq!(client.connection().transport().calls().len(), 2);
}

#[test]
fn a_method_that_is_not_safe_is_not_resent_and_the_next_call_reconnects() {
    let client = timed::Client::connect(mock(timed::SERVICE_NAME)).unwrap();
    client.connection().transport().fail(Errno::PIPE);
    assert_eq!(
        client.sync_now(),
        Err(CallError::Transport(Errno::PIPE)),
        "the service may have run it"
    );
    assert_eq!(client.connection().transport().calls().len(), 1);
    client.sync_now().unwrap();
    assert_eq!(client.connection().transport().resolves().len(), 2);
}
