//! The generated servers (`docs/client-api-plan.md` 3.3) driven through a mock
//! message: header checks, decoding, faults and their detail, objects, oneway,
//! and a round trip with the generated client over `Mock`.

use std::cell::RefCell;

use libmessenger::client::{remote_error, CallError, Connection, Errno, Mock};
use libmessenger::server::{Caller, Fault, Incoming};
use libmessenger::{Buffer, Encoder, Error, Object, Parcel};
use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::{
    os_lazy_clipboard_v1 as clipboard, os_lazy_confd_v1 as confd, os_lazy_echo_v1 as echo,
    os_lazy_timed_v1 as timed,
};

/// A received message with installed objects and a stamped caller.
struct Msg {
    interface: u64,
    method: u32,
    body: Vec<u8>,
    objects: RefCell<Vec<Object>>,
    caller: Caller,
}

impl Msg {
    fn new(interface: u64, method: u32, body: Vec<u8>) -> Msg {
        Msg {
            interface,
            method,
            body,
            objects: RefCell::new(Vec::new()),
            caller: Caller {
                uid: 1000,
                caps: 0x20,
                session: 7,
                sender: 3,
                ..Caller::default()
            },
        }
    }
}

impl Incoming for Msg {
    fn interface_id(&self) -> u64 {
        self.interface
    }
    fn method(&self) -> u32 {
        self.method
    }
    fn caller(&self) -> Caller {
        self.caller
    }
    fn decode<T>(
        &self,
        decoder: impl FnOnce(&[u8], &[Object]) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut objects = self.objects.borrow_mut();
        let value = decoder(&self.body, &objects)?;
        objects.clear();
        Ok(value)
    }
}

fn error_of(reply: &Parcel) -> (u32, String) {
    match remote_error(reply) {
        Some(CallError::Remote { code, message, .. }) => (code, message),
        other => panic!("not a refusal: {other:?}"),
    }
}

/// What a handler that will not serve a method answers.
fn refused<T>() -> Result<T, Fault> {
    Err(Fault::unserved())
}

#[derive(Default)]
struct Clock {
    zone: String,
    seen: Vec<Caller>,
}

impl timed::Server for Clock {
    fn set_time(&mut self, _: &Caller, _: i64) -> Result<(), Fault> {
        refused()
    }
    fn ntp_status(&mut self, _: &Caller) -> Result<timed::NtpStatusReply, Fault> {
        refused()
    }
    fn sync_now(&mut self, _: &Caller) -> Result<(), Fault> {
        refused()
    }
    fn now(&mut self, cx: &Caller) -> Result<timed::NowReply, Fault> {
        self.seen.push(*cx);
        Ok(timed::NowReply {
            unix_ms: 5,
            tz_offset_s: 3600,
            tz_name: self.zone.clone(),
            dst: true,
        })
    }
    fn get_zone(&mut self, _: &Caller) -> Result<String, Fault> {
        Ok(self.zone.clone())
    }
    fn set_zone(&mut self, cx: &Caller, name: String) -> Result<(), Fault> {
        if cx.caps & 0x1 == 0 {
            return Err(Fault::refuse(Errno::PERM, "only an administrator").with_hint("use elevd"));
        }
        self.zone = name;
        Ok(())
    }
}

fn timed_request(method: u32, body: Vec<u8>) -> Msg {
    Msg::new(timed::INTERFACE_ID, method, body)
}

#[test]
fn a_method_is_decoded_called_and_its_reply_encoded() {
    let mut clock = Clock {
        zone: "UTC".into(),
        ..Clock::default()
    };
    let reply = timed::dispatch(&mut clock, &timed_request(timed::METHOD_NOW, Vec::new()));
    assert_eq!(
        (reply.header.interface_id, reply.header.method),
        (timed::INTERFACE_ID, timed::METHOD_NOW)
    );
    let now = timed::decode_now_reply(&reply.body).unwrap();
    assert_eq!(
        (now.unix_ms, now.tz_offset_s, now.tz_name.as_str(), now.dst),
        (5, 3600, "UTC", true)
    );
    // The credentials are the stamped ones, not anything in the body.
    assert_eq!(clock.seen[0].uid, 1000);
    assert_eq!(clock.seen[0].sender, 3);
}

#[test]
fn a_one_field_reply_is_the_field() {
    let mut clock = Clock {
        zone: "Europe/Paris".into(),
        ..Clock::default()
    };
    let reply = timed::dispatch(
        &mut clock,
        &timed_request(timed::METHOD_GETZONE, Vec::new()),
    );
    assert_eq!(
        timed::decode_get_zone_reply(&reply.body).unwrap().name,
        "Europe/Paris"
    );
}

#[test]
fn an_unknown_method_is_enosys_and_a_wrong_interface_is_einval() {
    let mut clock = Clock::default();
    let reply = timed::dispatch(&mut clock, &timed_request(0xdead, Vec::new()));
    assert_eq!(error_of(&reply).0 as i64, Errno::NOSYS.number());
    let reply = timed::dispatch(&mut clock, &Msg::new(1, timed::METHOD_NOW, Vec::new()));
    assert_eq!(error_of(&reply).0 as i64, Errno::INVAL.number());
    assert!(
        clock.seen.is_empty(),
        "a refused header never reaches the handler"
    );
}

#[test]
fn a_bad_body_is_einval_and_the_handler_is_not_called() {
    let mut clock = Clock::default();
    // A set-zone request whose string field is truncated.
    let reply = timed::dispatch(
        &mut clock,
        &timed_request(timed::METHOD_SETZONE, vec![0xff, 0xff, 0xff]),
    );
    assert_eq!(error_of(&reply).0 as i64, Errno::INVAL.number());
    assert!(clock.zone.is_empty());
}

#[test]
fn a_fault_carries_its_code_message_and_detail() {
    let mut clock = Clock::default();
    let body = timed::encode_set_zone_args(&timed::SetZoneArgs { name: "UTC".into() }).unwrap();
    let reply = timed::dispatch(&mut clock, &timed_request(timed::METHOD_SETZONE, body));
    match remote_error(&reply) {
        Some(CallError::Remote {
            code,
            message,
            detail,
        }) => {
            assert_eq!(
                (code as i64, message.as_str()),
                (Errno::PERM.number(), "only an administrator")
            );
            assert_eq!(detail.hint, "use elevd");
        }
        other => panic!("{other:?}"),
    }
    assert!(reply.body.len() > 8 && ERROR_FIELD == 15);
}

struct Store;

impl confd::Server for Store {
    fn set(&mut self, _: &Caller, _: String, _: confd::Value) -> Result<(), Fault> {
        refused()
    }
    fn delete(&mut self, _: &Caller, _: String) -> Result<(), Fault> {
        refused()
    }
    fn list(&mut self, _: &Caller, _: String) -> Result<Vec<String>, Fault> {
        refused()
    }
    fn info(&mut self, _: &Caller) -> Result<confd::InfoReply, Fault> {
        refused()
    }
    fn get(&mut self, _: &Caller, path: String) -> Result<Option<confd::Value>, Fault> {
        match path.as_str() {
            "denied" => Err(confd::ServiceError::Denied.into()),
            _ => Err(Fault::from(confd::ServiceError::NotFound)),
        }
    }
}

#[test]
fn a_declared_service_error_keeps_its_code_and_sentence() {
    let body = confd::encode_get_args(&confd::GetArgs {
        path: "denied".into(),
    })
    .unwrap();
    let reply = confd::dispatch(
        &mut Store,
        &Msg::new(confd::INTERFACE_ID, confd::METHOD_GET, body),
    );
    let (code, message) = error_of(&reply);
    assert_eq!(
        confd::ServiceError::from_code(code),
        Some(confd::ServiceError::Denied)
    );
    assert_eq!(message, "The caller may not access that path.");
}

#[derive(Default)]
struct Pasteboard {
    fetched: Option<Buffer>,
}

impl clipboard::Server for Pasteboard {
    fn offer(
        &mut self,
        _: &Caller,
        _: String,
        _: Option<String>,
        _: Vec<String>,
        _: Vec<clipboard::Payload>,
    ) -> Result<u64, Fault> {
        refused()
    }
    fn offer_buffer(&mut self, _: &Caller, _: String, _: String, _: Buffer) -> Result<u64, Fault> {
        refused()
    }
    fn serialize(&mut self, _: &Caller, _: u64, _: String) -> Result<Vec<u8>, Fault> {
        refused()
    }
    fn ping(&mut self, _: &Caller) -> Result<(), Fault> {
        refused()
    }
    fn current(&mut self, _: &Caller) -> Result<Option<clipboard::OfferMeta>, Fault> {
        refused()
    }
    fn fetch(&mut self, _: &Caller, token: u64, _mime: String, into: Buffer) -> Result<u64, Fault> {
        self.fetched = Some(into);
        Ok(token + 1)
    }
    fn request(
        &mut self,
        _: &Caller,
        token: u64,
        _: String,
    ) -> Result<clipboard::RequestReply, Fault> {
        Ok(clipboard::RequestReply {
            token,
            bytes: vec![1, 2, 3],
            ..clipboard::RequestReply::default()
        })
    }
}

#[test]
fn objects_are_claimed_by_the_decode_and_passed_by_value() {
    let (body, objects) = clipboard::encode_fetch_args(&clipboard::FetchArgs {
        token: 9,
        mime: "text/plain".into(),
        into: Buffer::whole(77, 4096),
    })
    .unwrap();
    assert_eq!(objects.len(), 1);
    let message = Msg::new(clipboard::INTERFACE_ID, clipboard::METHOD_FETCH, body);
    // The kernel's installed handle replaces the sender's number.
    *message.objects.borrow_mut() = vec![Object::Buffer(77)];
    let mut board = Pasteboard::default();
    let reply = clipboard::dispatch(&mut board, &message);
    assert_eq!(
        clipboard::decode_fetch_reply(&reply.body).unwrap().written,
        10
    );
    assert_eq!(board.fetched.map(|b| b.handle), Some(77));
    assert!(
        message.objects.borrow().is_empty(),
        "claimed, so nothing is left to close"
    );
}

#[test]
fn a_request_missing_its_object_is_refused_and_keeps_it_for_the_message_to_close() {
    let (body, _) = clipboard::encode_fetch_args(&clipboard::FetchArgs {
        token: 9,
        mime: "text/plain".into(),
        into: Buffer::whole(1, 8),
    })
    .unwrap();
    let message = Msg::new(clipboard::INTERFACE_ID, clipboard::METHOD_FETCH, body);
    let mut board = Pasteboard::default();
    let reply = clipboard::dispatch(&mut board, &message);
    assert_eq!(error_of(&reply).0 as i64, Errno::INVAL.number());
    assert!(board.fetched.is_none());
}

#[test]
fn a_scoped_method_answers_on_its_scope_id_only() {
    let body = clipboard::encode_request_args(&clipboard::RequestArgs {
        token: 1,
        mime: "x".into(),
    })
    .unwrap();
    let mut board = Pasteboard::default();
    let ok = clipboard::dispatch(
        &mut board,
        &Msg::new(
            clipboard::METHOD_REQUEST_SCOPE,
            clipboard::METHOD_REQUEST,
            body.clone(),
        ),
    );
    assert_eq!(
        clipboard::decode_request_reply(&ok.body).unwrap().bytes,
        vec![1, 2, 3]
    );
    let wrong = clipboard::dispatch(
        &mut board,
        &Msg::new(clipboard::INTERFACE_ID, clipboard::METHOD_REQUEST, body),
    );
    // The interface id is ours but this method travels on its scope: no such method here.
    assert_eq!(error_of(&wrong).0 as i64, Errno::NOSYS.number());
}

#[derive(Default)]
struct Listener {
    heard: Vec<String>,
}

impl echo::Server for Listener {
    fn echo(&mut self, _: &Caller, _: String, _: u32) -> Result<String, Fault> {
        refused()
    }
    fn ping(&mut self, _: &Caller) -> Result<bool, Fault> {
        refused()
    }
    fn notify(&mut self, _: &Caller, event: echo::Event) {
        self.heard.push(event.topic);
    }
}

#[test]
fn a_oneway_method_calls_its_handler_and_yields_an_empty_reply() {
    let body = echo::encode_notify_args(&echo::NotifyArgs {
        event: echo::Event {
            topic: "t".into(),
            at: 1,
        },
    })
    .unwrap();
    let mut listener = Listener::default();
    let reply = echo::dispatch(
        &mut listener,
        &Msg::new(echo::INTERFACE_ID, echo::METHOD_NOTIFY, body),
    );
    assert_eq!(listener.heard, ["t"]);
    assert!(reply.body.is_empty());
}

/// Run the client's recorded request through a server and script the answer
/// back, so a generated client and a generated server meet over `Mock`.
fn serve_through(mock: &Mock, handle: impl FnOnce(&Msg) -> Parcel) {
    let call = mock.calls().pop().expect("a request was sent");
    mock.reply(handle(&Msg::new(
        call.header.interface_id,
        call.header.method,
        call.body,
    )));
}

#[test]
fn a_client_and_a_server_round_trip_through_the_mock() {
    let mock = Mock::new();
    mock.register(timed::SERVICE_NAME);
    let connection = Connection::new(mock, timed::SERVICE_NAME);
    let client = timed::Client::from_connection(connection);
    let mut clock = Clock {
        zone: "UTC".into(),
        ..Clock::default()
    };

    // The client's first call is scripted with the server's answer, which the
    // request has to exist for: record it with an empty script first.
    let transport = client.connection().transport();
    transport.reply(Parcel::default());
    let _ = client.now();
    serve_through(transport, |message| timed::dispatch(&mut clock, message));
    let now = client.now().unwrap();
    assert_eq!((now.tz_name.as_str(), now.dst), ("UTC", true));

    // A refusal reads back as the same code and sentence.
    transport.reply(Parcel::default());
    let _ = client.set_zone("Asia/Tokyo");
    serve_through(transport, |message| timed::dispatch(&mut clock, message));
    match client.set_zone("Asia/Tokyo") {
        Err(CallError::Remote {
            code,
            message,
            detail,
        }) => {
            assert_eq!(
                (code as i64, message.as_str()),
                (Errno::PERM.number(), "only an administrator")
            );
            assert_eq!(detail.hint, "use elevd");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_error_field_is_never_mistaken_for_a_reply_field() {
    // A refusal's body holds only the standard error, so a decoder of the
    // declared reply sees no field of its own.
    let reply = libmessenger::server::fault_reply(1, 1, &Fault::errno(Errno::BUSY));
    let mut body = Encoder::new();
    body.error(
        ERROR_FIELD,
        Errno::BUSY.number() as u32,
        Errno::BUSY.describe(),
    )
    .unwrap();
    assert_eq!(reply.body, body.finish());
}
