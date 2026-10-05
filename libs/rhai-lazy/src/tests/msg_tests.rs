//! The `msg` module against the in-memory fabric: calls, replies, errors,
//! the schema itself, and the codec cross-checked with the compiled codecs.

use alloc::rc::Rc;
use alloc::string::ToString;
use alloc::vec;

use messenger_generated::{
    os_lazy_echo_v1 as echo, os_lazy_messenger_registry_v1 as registry,
    os_lazy_messenger_topics_v1 as topics,
};
use rhai::{Dynamic, Map};

use super::msg_mock::{confd_service, echo_service, MockBus};
use super::{failure, run, value};
use crate::mock::MockHost;
use crate::msg::{codec, schema};
use crate::Outcome;

fn fabric() -> Rc<MockBus> {
    let bus = MockBus::new();
    echo_service(&bus);
    confd_service(&bus);
    bus
}

fn run_msg(bus: &Rc<MockBus>, source: &str) -> Outcome {
    run(MockHost::new().with_bus(bus.clone()), source).0
}

#[test]
fn method_sugar_calls_the_service() {
    let bus = fabric();
    let out = run_msg(
        &bus,
        r#"let e = msg::connect("os.lazy.echo.v1"); e.echo("ab", 3)"#,
    );
    assert_eq!(value(&out), "ababab");
    assert_eq!(
        value(&run_msg(&bus, r#"msg::connect("os.lazy.echo.v1").ping()"#)),
        "true"
    );
}

#[test]
fn generic_call_takes_positional_named_or_no_arguments() {
    let bus = fabric();
    let src = r#"
        let e = msg::connect("os.lazy.echo.v1");
        [e.invoke("Echo", ["x", 2]), e.invoke("echo", #{ count: 1, text: "y" }), e.invoke("Ping")]
    "#;
    assert_eq!(value(&run_msg(&bus, src)), r#"["xx", "y", true]"#);
}

#[test]
fn named_map_form_works_through_sugar_and_fills_zero_values() {
    let bus = fabric();
    let src = r#"msg::connect("os.lazy.echo.v1").echo(#{ text: "z" })"#;
    // `count` is absent, so it takes 0: the echo is empty.
    assert_eq!(value(&run_msg(&bus, src)), "");
}

#[test]
fn nested_option_struct_replies_decode_to_maps() {
    let bus = fabric();
    let src = r#"
        let c = msg::connect("os.lazy.confd.v1");
        let v = c.get("sys/ui/theme");
        [v.kind, v.str_value, v.i64_value, c.get("nope"), c.info().store_dir, c.info().persistent]
    "#;
    assert_eq!(
        value(&run_msg(&bus, src)),
        r#"[3, "dark", (), (), "/data/confd", true]"#
    );
}

#[test]
fn struct_arguments_are_encoded_from_maps() {
    let bus = fabric();
    let src = r#"msg::connect("os.lazy.confd.v1").set("sys/x", #{ kind: 1, i64_value: -5 })"#;
    assert_eq!(value(&run_msg(&bus, src)), "");
}

#[test]
fn a_service_refusal_is_a_catchable_friendly_error() {
    let bus = fabric();
    let src = r#"
        let c = msg::connect("os.lazy.confd.v1");
        let r = "no error";
        try { c.get("secret/key"); } catch (e) { r = e; }
        r
    "#;
    let text = value(&run_msg(&bus, src));
    assert!(text.contains("os.lazy.confd.v1.Get"), "{text}");
    assert!(text.contains("path is not readable by this user"), "{text}");
    assert!(text.contains("EACCES, code 13"), "{text}");
}

#[test]
fn bad_calls_fail_with_specific_messages() {
    let bus = fabric();
    let cases = [
        (
            r#"msg::connect("os.lazy.nope.v1")"#,
            "unknown interface `os.lazy.nope.v1`",
        ),
        (
            r#"msg::connect("os.lazy.timed.v1")"#,
            "no service for os.lazy.timed.v1",
        ),
        (
            r#"msg::connect("os.lazy.echo.v1").invoke("Nope")"#,
            "has no method `Nope`",
        ),
        (
            r#"msg::connect("os.lazy.echo.v1").echo(1, 2)"#,
            "text: expected a string, got i64",
        ),
        (
            r#"msg::connect("os.lazy.echo.v1").echo("a", -1)"#,
            "count: -1 is out of range",
        ),
        (
            r#"msg::connect("os.lazy.echo.v1").echo("a")"#,
            "expected 2 argument(s), got 1",
        ),
        (
            r#"msg::connect("os.lazy.echo.v1").echo(#{ txt: "a" })"#,
            "unknown field `txt`",
        ),
        (
            r#"msg::connect("os.lazy.echo.v1").invoke_oneway("Echo", ["a", 1])"#,
            "not a one-way method",
        ),
        (
            r#"msg::connect("os.lazy.confd.v1").echo("a", 1)"#,
            "os.lazy.confd.v1 has no method `echo`",
        ),
        (r#"msg::describe("x")"#, "unknown interface `x`"),
    ];
    for (source, expected) in cases {
        let message = failure(&run_msg(&bus, source));
        assert!(message.contains(expected), "{source}: {message}");
    }
}

#[test]
fn one_way_methods_are_sent_not_called() {
    let bus = fabric();
    let src = r#"msg::connect("os.lazy.echo.v1").notify(#{ topic: "t", at: 7 })"#;
    assert_eq!(value(&run_msg(&bus, src)), "");
    let sent = bus.sent.borrow();
    assert_eq!(sent.len(), 1);
    let (_, iface, method, body) = &sent[0];
    assert_eq!((*iface, *method), (echo::INTERFACE_ID, echo::METHOD_NOTIFY));
    let args = echo::decode_notify_args(body).unwrap();
    assert_eq!((args.event.topic.as_str(), args.event.at), ("t", 7));
}

#[test]
fn connect_falls_back_to_the_full_name_and_accepts_an_explicit_one() {
    let bus = MockBus::new();
    bus.serve("os.lazy.echo.v1", |_, _, _| {
        echo::encode_ping_reply(&echo::PingReply { alive: true }).map_err(|_| unreachable!())
    });
    bus.serve("demo.echo", |_, _, _| {
        echo::encode_ping_reply(&echo::PingReply { alive: false }).map_err(|_| unreachable!())
    });
    let src = r#"
        let a = msg::connect("os.lazy.echo.v1");
        let b = msg::connect("os.lazy.echo.v1", "demo.echo");
        [a.service, a.ping(), b.service, b.ping(), b.interface]
    "#;
    assert_eq!(
        value(&run_msg(&bus, src)),
        r#"["os.lazy.echo.v1", true, "demo.echo", false, "os.lazy.echo.v1"]"#
    );
}

#[test]
fn a_dead_peer_is_resolved_again_once() {
    let bus = fabric();
    let src = r#"
        let e = msg::connect("os.lazy.echo.v1");
        e.ping()
    "#;
    bus.kill_once.set(Some(100));
    assert_eq!(value(&run_msg(&bus, src)), "true");
    // connect (1) + the re-resolve after EPIPE (1).
    assert_eq!(bus.resolves.get(), 2);
}

#[test]
fn a_timeout_is_reported_not_hung() {
    let bus = fabric();
    bus.stalled.set(true);
    let message = failure(&run_msg(
        &bus,
        r#"msg::set_timeout(10); msg::connect("os.lazy.echo.v1").ping()"#,
    ));
    assert!(message.contains("timed out (ETIMEDOUT)"), "{message}");
}

#[test]
fn introspection_lists_interfaces_services_and_signatures() {
    let bus = fabric();
    let src = r#"[msg::interfaces().contains("os.lazy.echo.v1"), msg::services(), msg::timeout()]"#;
    assert_eq!(
        value(&run_msg(&bus, src)),
        r#"[true, ["os.lazy.confd", "os.lazy.echo"], 5000]"#
    );
    let text = value(&run_msg(&bus, r#"msg::describe("os.lazy.echo.v1")"#));
    assert!(
        text.contains("Echo(text: String, count: U32) -> (reply: String)"),
        "{text}"
    );
    assert!(text.contains("Notify(event: Event) -> () oneway"), "{text}");
    let methods = value(&run_msg(&bus, r#"msg::connect("os.lazy.echo.v1").methods"#));
    assert_eq!(methods, r#"["Echo", "Ping", "Notify"]"#);
}

#[test]
fn without_a_fabric_there_is_no_msg_module() {
    let (out, _) = run(MockHost::new(), r#"msg::interfaces()"#);
    assert!(failure(&out).contains("msg"), "{out:?}");
}

#[test]
fn soak_many_calls_reuse_one_endpoint() {
    let bus = fabric();
    let src = r#"
        let e = msg::connect("os.lazy.echo.v1");
        let n = 0;
        for i in 0..5000 { n += e.echo("ab", i % 4).len(); }
        n
    "#;
    // Each block of 4 iterations echoes 0+2+4+6 bytes.
    assert_eq!(value(&run_msg(&bus, src)), (5000 / 4 * 12).to_string());
    assert_eq!(bus.resolves.get(), 1);
    assert_eq!(bus.calls.get(), 5000);
}

#[test]
fn schema_matches_the_compiled_codecs() {
    let iface = schema::interface("os.lazy.echo.v1").unwrap();
    assert_eq!(iface.id, echo::INTERFACE_ID);
    assert_eq!(iface.method("Echo").unwrap().id, echo::METHOD_ECHO);
    assert_eq!(iface.method("notify").unwrap().id, echo::METHOD_NOTIFY);
    let topics_iface = schema::interface("os.lazy.messenger.topics.v1").unwrap();
    assert_eq!(topics_iface.id, topics::INTERFACE_ID);
    assert_eq!(
        topics_iface.method("list_topics").unwrap().id,
        topics::METHOD_LISTTOPICS
    );
    for iface in schema::interfaces() {
        for (i, a) in iface.methods.iter().enumerate() {
            assert!(
                iface.methods[i + 1..].iter().all(|b| b.id != a.id),
                "{}",
                iface.name
            );
        }
    }
}

#[test]
fn dynamic_encoding_decodes_with_the_compiled_codec() {
    let iface = schema::interface("os.lazy.messenger.registry.v1").unwrap();
    let method = iface.method("Register").unwrap();
    let big = 0xcc4a_c105_7e84_db93u64; // above i64::MAX
    let args = vec![
        Dynamic::from("svc".to_string()),
        Dynamic::from_int(7),
        Dynamic::from_array(vec![Dynamic::from_int(big as i64), Dynamic::from_int(1)]),
        Dynamic::from_int(0),
        Dynamic::from_array(vec![Dynamic::from("com.x.chat.v1".to_string())]),
    ];
    let body = codec::encode_positional(iface, method.params, &args).unwrap();
    let decoded = registry::decode_register_args(&body).unwrap();
    assert_eq!(decoded.name, "svc");
    assert_eq!(decoded.endpoint, Some(7));
    assert_eq!(decoded.interfaces, vec![big, 1]);
    assert_eq!(decoded.interface_names, vec!["com.x.chat.v1"]);
}

#[test]
fn compiled_encoding_decodes_dynamically() {
    let iface = schema::interface("os.lazy.messenger.topics.v1").unwrap();
    let reply = topics::ListTopicsReply {
        topics: vec![topics::TopicInfo {
            topic: "a/b".into(),
            subscribers: 2,
            retained: true,
        }],
    };
    let body = topics::encode_list_topics_reply(&reply).unwrap();
    let method = iface.method("ListTopics").unwrap();
    let map: Map = codec::decode_named(iface, method.returns, &body, 0).unwrap();
    assert_eq!(
        map["topics"].to_string(),
        r#"[#{"retained": true, "subscribers": 2, "topic": "a/b"}]"#
    );
}

#[test]
fn malformed_replies_and_handles_are_errors_not_panics() {
    let iface = schema::interface("os.lazy.echo.v1").unwrap();
    let method = iface.method("Echo").unwrap();
    assert!(codec::decode_named(iface, method.returns, &[7, 1, 0, 0, 9, 0, 0, 0], 0).is_err());
    // A string field whose bytes are not UTF-8.
    let mut body = libmessenger::Encoder::new();
    body.bytes(1, &[0xff, 0xfe]).unwrap();
    let mut raw = body.finish();
    raw[0] = 7; // relabel the kind as String
    assert!(codec::decode_named(iface, method.returns, &raw, 0).is_err());
    let reg = schema::interface("os.lazy.display.v1").unwrap();
    let handles = reg
        .methods
        .iter()
        .find(|m| m.params.iter().any(|f| f.ty == schema::Ty::Handle));
    if let Some(m) = handles {
        let args: alloc::vec::Vec<Dynamic> =
            m.params.iter().map(|_| Dynamic::from_int(1)).collect();
        let error = codec::encode_positional(reg, m.params, &args).unwrap_err();
        assert!(error.contains("cannot send handles"), "{error}");
    }
}

#[test]
fn snake_case_and_topic_matching() {
    assert_eq!(schema::snake_case("ListTopics"), "list_topics");
    assert_eq!(schema::snake_case("Get"), "get");
    assert!(schema::topic_matches(
        "system/confd/changed/#",
        "system/confd/changed/sys/a"
    ));
    assert!(schema::topic_matches("a/#", "a"));
    assert!(schema::topic_matches(
        "session/+/clipboard/changed",
        "session/3/clipboard/changed"
    ));
    assert!(!schema::topic_matches(
        "session/+/clipboard/changed",
        "session/3/clipboard"
    ));
    assert!(!schema::topic_matches("a/b", "a/b/c"));
    let iface = schema::interface("os.lazy.confd.v1").unwrap();
    assert_eq!(iface.default_service(), "os.lazy.confd");
    assert!(schema::declared_topic("system/confd/changed/sys/ui/theme").is_some());
}

#[test]
fn engines_sharing_a_fabric_resolve_a_service_once() {
    let bus = fabric();
    let shared = Rc::new(crate::msg::Fabric::new(bus.clone()));
    for _ in 0..3 {
        let mut engine = rhai::Engine::new();
        crate::msg::install_fabric(&mut engine, &shared).unwrap();
        let alive: bool = engine
            .eval(r#"msg::connect("os.lazy.echo.v1").ping()"#)
            .unwrap();
        assert!(alive);
    }
    assert_eq!(bus.resolves.get(), 1);
}
