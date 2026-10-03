//! A host-driven event loop (`msg::events`): what a LazyRAD form's window
//! does with `Fabric::pump` / `release` instead of `msg::run()`.

use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use messenger_generated::os_lazy_echo_v1 as echo;
use rhai::{Dynamic, Engine, FnPtr, Scope, AST};

use super::msg_mock::{topics_service, MockBus};
use crate::msg::{codec, events::PER_SOURCE, schema, Fabric, Incoming, Pumped, Wait};

/// One form: an engine bound to `owner`, its compiled script, and what its
/// handlers recorded through `note(x)`.
struct Form {
    engine: Engine,
    ast: AST,
    notes: Rc<RefCell<Vec<String>>>,
}

impl Form {
    fn new(fabric: &Rc<Fabric>, owner: &str, script: &str) -> Form {
        let mut engine = Engine::new();
        crate::msg::install_hosted(&mut engine, fabric, owner).unwrap();
        let notes = Rc::new(RefCell::new(Vec::new()));
        let sink = notes.clone();
        engine.register_fn("note", move |x: Dynamic| sink.borrow_mut().push(x.to_string()));
        let ast = engine.compile(script).unwrap();
        engine.run_ast_with_scope(&mut Scope::new(), &ast).unwrap();
        Form { engine, ast, notes }
    }

    /// One window tick: what `lazyrad_runtime` does on its timer.
    fn pump(&self, fabric: &Fabric, owner: &str) -> Pumped {
        let mut call = |f: &FnPtr, args: Vec<Dynamic>| f.call::<Dynamic>(&self.engine, &self.ast, args);
        fabric.pump(owner, &mut call)
    }

    fn notes(&self) -> Vec<String> {
        self.notes.borrow().clone()
    }
}

fn setup() -> (Rc<MockBus>, Rc<Fabric>) {
    let bus = MockBus::new();
    topics_service(&bus);
    let fabric = Rc::new(Fabric::new(bus.clone()));
    (bus, fabric)
}

fn publish(fabric: &Rc<Fabric>, topic: &str, text: &str) {
    let mut engine = Engine::new();
    crate::msg::install_fabric(&mut engine, fabric).unwrap();
    engine
        .run(&alloc::format!(r#"msg::publish("{topic}", "{text}");"#))
        .unwrap();
}

#[test]
fn a_pump_runs_only_its_own_forms_handlers() {
    let (_bus, fabric) = setup();
    let main = Form::new(&fabric, "main", r#"msg::on("demo/a", |e| note(e.payload.as_string()));"#);
    let other = Form::new(&fabric, "other", r#"msg::on("demo/b", |e| note("b"));"#);
    publish(&fabric, "demo/a", "hello");
    publish(&fabric, "demo/b", "x");

    let pumped = main.pump(&fabric, "main");
    assert_eq!(pumped.handled, 1);
    assert_eq!(main.notes(), ["hello"]);
    assert!(other.notes().is_empty(), "main's tick ran other's handler");

    assert_eq!(other.pump(&fabric, "other").handled, 1);
    assert_eq!(other.notes(), ["b"]);
    assert_eq!(main.pump(&fabric, "main").handled, 0, "nothing left");
}

#[test]
fn a_pump_never_waits() {
    let (bus, fabric) = setup();
    let form = Form::new(&fabric, "main", r#"msg::on("demo/a", |e| ());"#);
    bus.waits.borrow_mut().clear();
    let before = bus.clock.get();
    assert_eq!(form.pump(&fabric, "main").handled, 0);
    assert_eq!(bus.waits.borrow().as_slice(), [Wait::Poll]);
    assert_eq!(bus.clock.get(), before, "an empty poll advanced the clock");
}

#[test]
fn generated_topic_helpers_deliver_decoded_payloads_through_the_pump() {
    let (_bus, fabric) = setup();
    let form = Form::new(
        &fabric,
        "main",
        r#"sys::confd::on_changed("sys/ui/#", |e| note(e.payload.path + ":" + e.payload.deleted));"#,
    );
    let mut engine = Engine::new();
    crate::msg::install_fabric(&mut engine, &fabric).unwrap();
    engine
        .run(r#"sys::confd::publish_changed("sys/ui/theme", #{ path: "sys/ui/theme", deleted: true });
               sys::confd::publish_changed("net/x", #{ path: "net/x" });"#)
        .unwrap();
    form.pump(&fabric, "main");
    assert_eq!(form.notes(), ["sys/ui/theme:true"]);
}

#[test]
fn one_pump_takes_a_bounded_number_of_events_per_source() {
    let (_bus, fabric) = setup();
    let form = Form::new(&fabric, "main", r#"msg::on("demo/a", #{ qos: "buffered", depth: 64 }, |e| ());"#);
    for n in 0..PER_SOURCE + 3 {
        publish(&fabric, "demo/a", &n.to_string());
    }
    assert_eq!(form.pump(&fabric, "main").handled, PER_SOURCE);
    assert_eq!(form.pump(&fabric, "main").handled, 3);
}

#[test]
fn a_failing_handler_is_reported_once_per_run_of_failures() {
    let (_bus, fabric) = setup();
    let form = Form::new(
        &fabric,
        "main",
        r#"msg::on("demo/a", #{ qos: "buffered", depth: 8 }, |e| {
               if e.payload.as_string() == "bad" { throw "cannot handle " + e.payload.as_string(); }
               note("ok");
           });"#,
    );
    publish(&fabric, "demo/a", "bad");
    let first = form.pump(&fabric, "main");
    assert_eq!((first.errors.len(), first.suppressed), (1, 0));
    assert!(first.errors[0].to_string().contains("cannot handle bad"));

    publish(&fabric, "demo/a", "bad");
    let again = form.pump(&fabric, "main");
    assert_eq!((again.errors.len(), again.suppressed), (0, 1), "a repeat is counted, not shown");

    publish(&fabric, "demo/a", "fine");
    publish(&fabric, "demo/a", "bad");
    let recovered = form.pump(&fabric, "main");
    assert_eq!(form.notes(), ["ok"]);
    assert_eq!(recovered.errors.len(), 1, "a failure after a success is shown again");
}

#[test]
fn a_reliable_event_is_acked_even_when_its_handler_throws() {
    let bus = MockBus::new();
    let acks = topics_service(&bus);
    let fabric = Rc::new(Fabric::new(bus));
    let form = Form::new(&fabric, "main", r#"msg::on("demo/r", #{ qos: "reliable" }, |e| throw "no");"#);
    publish(&fabric, "demo/r", "x");
    assert_eq!(form.pump(&fabric, "main").errors.len(), 1);
    assert_eq!(acks.borrow().as_slice(), [1]);
}

/// A call to `os.lazy.echo.v1.Echo` queued on the first served endpoint.
fn inject_echo(bus: &MockBus, txn: u64, text: &str) {
    let iface = schema::interface("os.lazy.echo.v1").unwrap();
    let method = iface.method("Echo").unwrap();
    let args = [Dynamic::from(String::from(text)), Dynamic::from_int(2)];
    let body = codec::encode_positional(iface, method.params, &args).unwrap();
    bus.inbox.borrow_mut().push_back((
        900,
        Incoming {
            interface: echo::INTERFACE_ID,
            method: echo::METHOD_ECHO,
            txn: Some(txn),
            body,
        },
    ));
}

#[test]
fn a_form_serves_calls_from_its_window_and_releases_the_name() {
    let (bus, fabric) = setup();
    let form = Form::new(
        &fabric,
        "main",
        r#"msg::serve("demo.form", "os.lazy.echo.v1", #{ Echo: |text, count| { note(text); text + "!" } });"#,
    );
    inject_echo(&bus, 41, "hi");
    assert_eq!(form.pump(&fabric, "main").handled, 1);
    assert_eq!(form.notes(), ["hi"]);
    let replies = bus.replies.borrow();
    let reply = echo::decode_echo_reply(&replies[0].1).unwrap();
    assert_eq!((replies[0].0, reply.reply.as_str()), (41, "hi!"));
    drop(replies);

    assert_eq!(fabric.release("main"), 1);
    assert_eq!(bus.unregistered.borrow().as_slice(), [("demo.form".to_string(), 900)]);
    assert!(!fabric.active("main"));
}

#[test]
fn release_closes_subscriptions_and_keeps_other_forms() {
    let (_bus, fabric) = setup();
    let main = Form::new(&fabric, "main", r#"msg::on("demo/a", |e| note("main"));"#);
    let other = Form::new(&fabric, "other", r#"msg::on("demo/a", |e| note("other"));"#);
    assert_eq!(fabric.release("main"), 1);
    publish(&fabric, "demo/a", "x");
    assert_eq!(main.pump(&fabric, "main").handled, 0);
    assert_eq!(other.pump(&fabric, "other").handled, 1);
    assert_eq!(other.notes(), ["other"]);
    assert!(main.notes().is_empty());
}

#[test]
fn closing_a_subscription_stops_its_handler() {
    let (_bus, fabric) = setup();
    let form = Form::new(
        &fabric,
        "main",
        r#"let s = msg::on("demo/a", |e| note("seen")); s.close();"#,
    );
    publish(&fabric, "demo/a", "x");
    assert!(!fabric.active("main"));
    assert_eq!(form.pump(&fabric, "main").handled, 0);
}

#[test]
fn msg_run_refuses_in_a_hosted_engine() {
    let (_bus, fabric) = setup();
    let mut engine = Engine::new();
    crate::msg::install_hosted(&mut engine, &fabric, "main").unwrap();
    let error = engine
        .run(r#"msg::on("demo/a", |e| ()); msg::run();"#)
        .unwrap_err()
        .to_string();
    assert!(error.contains("not needed here"), "{error}");
}

#[test]
fn a_soak_of_many_ticks_and_forms_leaks_no_sources() {
    let (_bus, fabric) = setup();
    for round in 0..200 {
        let owner = alloc::format!("form{}", round % 7);
        let form = Form::new(&fabric, &owner, r#"msg::on("demo/s", |e| note("x"));"#);
        publish(&fabric, "demo/s", "x");
        assert_eq!(form.pump(&fabric, &owner).handled, 1, "round {round}");
        assert_eq!(fabric.release(&owner), 1, "round {round}");
    }
    assert!(fabric.sources.borrow().is_empty());
}
