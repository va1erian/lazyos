//! Topics, the event loop and services written in Rhai, against the
//! in-memory fabric (whose broker and clients use the compiled codecs).

use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use messenger_generated::{os_lazy_confd_v1 as confd, os_lazy_echo_v1 as echo};

use super::msg_mock::{topics_service, MockBus};
use super::{failure, run, value};
use crate::mock::MockHost;
use crate::msg::{codec, Incoming};
use crate::Outcome;

fn fabric() -> Rc<MockBus> {
    let bus = MockBus::new();
    topics_service(&bus);
    bus
}

fn run_msg(bus: &Rc<MockBus>, source: &str) -> Outcome {
    run(MockHost::new().with_bus(bus.clone()), source).0
}

/// Queue a call to the first endpoint a script serves (the mock numbers it 900).
fn inject(bus: &MockBus, method: u32, txn: Option<u64>, body: Vec<u8>) {
    bus.inbox.borrow_mut().push_back((
        900,
        Incoming {
            interface: echo::INTERFACE_ID,
            method,
            txn,
            body,
        },
    ));
}

#[test]
fn declared_topics_encode_and_decode_their_payload_type() {
    let bus = fabric();
    let src = r#"
        let s = msg::subscribe("system/confd/changed/#");
        let n = msg::publish("system/confd/changed/sys/x", #{ path: "sys/x", deleted: true });
        let e = s.next(100);
        [n, e.topic, e.payload.path, e.payload.deleted, e.sequence, type_of(e.bytes)]
    "#;
    assert_eq!(
        value(&run_msg(&bus, src)),
        r#"[1, "system/confd/changed/sys/x", "sys/x", true, 1, "blob"]"#
    );
}

#[test]
fn the_wire_payload_is_what_the_compiled_codec_writes() {
    let bus = fabric();
    let src = r#"
        let s = msg::subscribe("system/confd/changed/#");
        msg::publish("system/confd/changed/sys/y", #{ path: "sys/y" });
        s.next(100).bytes
    "#;
    let Outcome::Value(bytes) = run_msg(&bus, src) else {
        panic!("expected a value")
    };
    let change = confd::decode_change(&bytes.cast::<rhai::Blob>()).unwrap();
    assert_eq!((change.path.as_str(), change.deleted), ("sys/y", false));
}

#[test]
fn undeclared_topics_carry_bytes_and_next_times_out_to_unit() {
    let bus = fabric();
    let src = r#"
        let s = msg::subscribe("demo/+/ping", #{ qos: "buffered", depth: 4 });
        msg::publish("demo/a/ping", "hello");
        let e = s.next(50);
        [e.payload.len(), s.next(30), s.filter, msg::publish("demo/none", "x")]
    "#;
    assert_eq!(value(&run_msg(&bus, src)), r#"[5, (), "demo/+/ping", 0]"#);
}

#[test]
fn bad_topic_calls_fail_with_specific_messages() {
    let bus = fabric();
    let cases = [
        (r#"msg::publish("demo/x", 42)"#, "must be a blob or string"),
        (
            r#"msg::publish("system/confd/changed/sys/x", "text")"#,
            "expected an object map",
        ),
        (
            r#"msg::publish("system/confd/changed/sys/x", #{ pth: "a" })"#,
            "unknown field `pth`",
        ),
        (
            r#"msg::subscribe("a/#", #{ qos: "fast" })"#,
            "qos `fast` is not one of",
        ),
        (
            r#"msg::subscribe("a/#", #{ depth: 0 })"#,
            "depth must be 1..=64",
        ),
        (
            r#"msg::subscribe("a/#", #{ deep: 1 })"#,
            "unknown option `deep`",
        ),
        ("msg::run(10)", "nothing to wait for"),
    ];
    for (source, expected) in cases {
        let message = failure(&run_msg(&bus, source));
        assert!(message.contains(expected), "{source}: {message}");
    }
}

#[test]
fn on_and_run_dispatch_events_to_closures() {
    let bus = fabric();
    let src = r#"
        let seen = [];
        msg::on("demo/#", |e| seen.push(e.topic));
        msg::publish("demo/a", "1");
        msg::publish("demo/b", "2");
        msg::publish("other", "3");
        let handled = msg::run(100);
        [handled, seen]
    "#;
    assert_eq!(value(&run_msg(&bus, src)), r#"[2, ["demo/a", "demo/b"]]"#);
}

#[test]
fn stop_ends_an_unbounded_run_and_reliable_events_are_acked() {
    let bus = MockBus::new();
    let acks = topics_service(&bus);
    let src = r#"
        msg::on("demo/#", #{ qos: "reliable" }, |e| { if e.payload.len() == 4 { msg::stop() } });
        msg::publish("demo/a", "one");
        msg::publish("demo/a", "stop");
        msg::publish("demo/a", "never");
        msg::run()
    "#;
    assert_eq!(value(&run_msg(&bus, src)), "2");
    assert_eq!(*acks.borrow(), [1, 2]);
}

#[test]
fn a_failing_topic_handler_ends_the_loop_with_its_error() {
    let bus = fabric();
    let src =
        r#"msg::on("demo/#", |e| throw "bad event"); msg::publish("demo/a", "x"); msg::run(50)"#;
    assert!(failure(&run_msg(&bus, src)).contains("bad event"));
}

#[test]
fn a_rhai_service_answers_with_the_compiled_reply_shape() {
    let bus = fabric();
    let args = echo::encode_echo_args(&echo::EchoArgs {
        text: "hi".into(),
        count: 3,
    })
    .unwrap();
    inject(&bus, echo::METHOD_ECHO, Some(5), args);
    inject(&bus, echo::METHOD_PING, Some(6), Vec::new());
    let src = r#"
        msg::serve("demo.echo", "os.lazy.echo.v1", #{
            Echo: |text, count| { let r = ""; for i in 0..count { r += text; } r },
            ping: || true,
        });
        msg::run(50)
    "#;
    assert_eq!(value(&run_msg(&bus, src)), "2");
    let registered = bus.registered.borrow();
    assert_eq!(registered[0].0, "demo.echo");
    assert_eq!(registered[0].2, [echo::INTERFACE_ID]);
    let replies = bus.replies.borrow();
    assert_eq!(replies[0].0, 5);
    assert_eq!(
        echo::decode_echo_reply(&replies[0].1).unwrap().reply,
        "hihihi"
    );
    assert!(echo::decode_ping_reply(&replies[1].1).unwrap().alive);
}

#[test]
fn handler_errors_and_missing_handlers_become_structured_errors() {
    let bus = fabric();
    let args = echo::encode_echo_args(&echo::EchoArgs {
        text: "x".into(),
        count: 1,
    })
    .unwrap();
    inject(&bus, echo::METHOD_ECHO, Some(1), args.clone());
    inject(&bus, echo::METHOD_ECHO, Some(2), args);
    inject(&bus, echo::METHOD_PING, Some(3), Vec::new());
    let src = r#"
        let calls = 0;
        msg::serve("os.lazy.echo.v1", #{
            Echo: |text, count| {
                calls += 1;
                if calls == 1 { throw "no echo today" }
                throw #{ code: 13, message: "denied" };
            },
        });
        msg::run(50)
    "#;
    assert_eq!(value(&run_msg(&bus, src)), "3");
    assert_eq!(bus.registered.borrow()[0].0, "os.lazy.echo");
    let replies = bus.replies.borrow();
    let errors: Vec<(u32, String)> = replies
        .iter()
        .map(|(_, body)| codec::reply_error(body).unwrap())
        .collect();
    assert_eq!(errors[0].0, 5);
    assert!(errors[0].1.contains("no echo today"), "{}", errors[0].1);
    assert_eq!(errors[1], (13, "denied".to_string()));
    assert_eq!(errors[2].0, 38);
}

#[test]
fn one_way_requests_run_the_handler_and_send_no_reply() {
    let bus = fabric();
    let args = echo::encode_notify_args(&echo::NotifyArgs {
        event: echo::Event {
            topic: "t".into(),
            at: 9,
        },
    })
    .unwrap();
    inject(&bus, echo::METHOD_NOTIFY, None, args);
    let src = r#"
        let got = ();
        msg::serve("demo.echo", "os.lazy.echo.v1", #{ notify: |event| { got = event.at; msg::stop(); } });
        msg::run();
        got
    "#;
    assert_eq!(value(&run_msg(&bus, src)), "9");
    assert!(bus.replies.borrow().is_empty());
}

#[test]
fn serve_rejects_unknown_methods_and_non_functions() {
    let bus = fabric();
    let cases = [
        (
            r#"msg::serve("x", "os.lazy.echo.v1", #{ shout: || 1 })"#,
            "has no method `shout`",
        ),
        (
            r#"msg::serve("x", "os.lazy.echo.v1", #{ echo: 1 })"#,
            "is not a function",
        ),
        (
            r#"msg::serve("x", "os.lazy.nope.v1", #{})"#,
            "unknown interface",
        ),
        (
            r#"msg::serve("os.lazy.messenger.topics", "os.lazy.echo.v1", #{})"#,
            "cannot register",
        ),
    ];
    for (source, expected) in cases {
        let message = failure(&run_msg(&bus, source));
        assert!(message.contains(expected), "{source}: {message}");
    }
}

#[test]
fn soak_thousands_of_events_through_the_loop() {
    let bus = fabric();
    let src = r#"
        let total = 0;
        msg::on("soak/#", |e| total += e.payload.len());
        for i in 0..2000 { msg::publish("soak/" + (i % 7), "abc"); }
        let handled = msg::run(1000000);
        [handled, total]
    "#;
    assert_eq!(value(&run_msg(&bus, src)), "[2000, 6000]");
}

#[test]
fn payloads_travel_in_the_platform_wrapper_parcel() {
    use crate::msg::topics::{unwrap, wrap};
    use libmessenger::{Decoder, Kind, Parcel};
    let text = Parcel::decode(&wrap(b"hi").unwrap()).unwrap();
    assert_eq!(text.header.interface_id, u64::from_le_bytes(*b"os.cntrl"));
    let field = Decoder::new(&text.body).next().unwrap().unwrap();
    assert_eq!(
        (field.kind, field.id, field.as_bytes()),
        (Kind::String, 1, &b"hi"[..])
    );
    let binary = Parcel::decode(&wrap(&[0xff, 0]).unwrap()).unwrap();
    let field = Decoder::new(&binary.body).next().unwrap().unwrap();
    assert_eq!((field.kind, field.id), (Kind::Bytes, 2));
    assert_eq!(unwrap(&wrap(&[0xff, 0]).unwrap()), [0xff, 0]);
    // Not a wrapper: handed through unchanged.
    assert_eq!(unwrap(b"raw"), b"raw");
}
