//! The generated `sys::*` modules (`api/*.rhai`, `msg::api`) against the
//! in-memory fabric: each generated function must do exactly what the
//! dynamic `msg` call it wraps does.

use alloc::rc::Rc;

use rhai::{Engine, Module, Scope};

use super::msg_mock::{confd_service, echo_service, topics_service, MockBus};
use super::{failure, run, value};
use crate::mock::MockHost;
use crate::msg::{api, schema, Fabric};
use crate::Outcome;

fn fabric() -> Rc<MockBus> {
    let bus = MockBus::new();
    echo_service(&bus);
    confd_service(&bus);
    topics_service(&bus);
    bus
}

fn run_msg(bus: &Rc<MockBus>, source: &str) -> Outcome {
    run(MockHost::new().with_bus(bus.clone()), source).0
}

#[test]
fn every_scriptable_interface_has_a_module_and_kernel_scopes_have_none() {
    for interface in schema::interfaces() {
        let module = api::modules().iter().find(|m| m.interface == interface.name);
        let kernel_scope = interface.name.contains(".topics.publish.")
            || interface.name.contains(".topics.subscribe.")
            || interface.name.contains(".names.resolve.")
            || interface.name.contains(".messenger.policy.")
            || interface.name.contains(".process.label.spawn.");
        assert_eq!(module.is_none(), kernel_scope, "{}", interface.name);
    }
    let confd = api::module("confd").expect("sys::confd");
    assert_eq!(confd.interface, "os.lazy.confd.v1");
    assert_eq!(confd.topics[0].helper, "changed");
    assert_eq!(confd.topics[0].pattern, "system/confd/changed/#");
}

#[test]
fn every_module_compiles_and_evaluates() {
    let engine = Engine::new();
    let sys = api::build(&engine).expect("the generated modules compile");
    for module in api::modules() {
        assert!(sys.get_sub_module(module.alias).is_some(), "sys::{}", module.alias);
    }
}

#[test]
fn compiling_every_module_stays_cheap() {
    // The player pays this once per process; under TCG it is several times
    // slower than here, so keep a wide margin (release host builds take a few
    // milliseconds, debug ones well under a second).
    let engine = Engine::new();
    let started = std::time::Instant::now();
    api::build(&engine).unwrap();
    let elapsed = started.elapsed();
    std::println!("sys::* compiled in {elapsed:?}");
    assert!(elapsed.as_millis() < 1_000, "compiling sys::* took {elapsed:?}");
}

#[test]
fn methods_call_the_service_like_msg_does() {
    let bus = fabric();
    let src = r#"
        [sys::echo::echo("ab", 3),
         sys::echo::ping(),
         sys::confd::info().store_dir,
         sys::confd::get("sys/ui/theme").str_value,
         sys::confd::get("absent"),
         sys::confd::connect().interface,
         sys::confd::INTERFACE]
    "#;
    assert_eq!(
        value(&run_msg(&bus, src)),
        r#"["ababab", true, "/data/confd", "dark", (), "os.lazy.confd.v1", "os.lazy.confd.v1"]"#
    );
}

#[test]
fn struct_constructors_give_every_field_and_feed_calls() {
    let bus = fabric();
    let src = r#"
        let v = sys::confd::new_value();
        let keys = v.keys();
        keys.sort();
        v.kind = 1;
        v.i64_value = -5;
        sys::confd::set("sys/test/n", v);
        [keys, sys::confd::new_change()]
    "#;
    assert_eq!(
        value(&run_msg(&bus, src)),
        r#"[["bool_value", "bytes_value", "i64_value", "kind", "str_value", "u64_value"], #{"deleted": false, "path": ""}]"#
    );
}

#[test]
fn service_errors_keep_their_text() {
    let bus = fabric();
    let message = failure(&run_msg(&bus, r#"sys::confd::get("secret/x")"#));
    assert!(message.contains("os.lazy.confd.v1.Get"), "{message}");
    assert!(message.contains("EACCES"), "{message}");
}

#[test]
fn enums_are_constants_with_their_variant_names() {
    let bus = fabric();
    let src = r#"
        [sys::messenger_topics::QOS_RELIABLE, sys::messenger_topics::QOS.len(),
         sys::messenger_topics::QOS[0]]
    "#;
    assert_eq!(value(&run_msg(&bus, src)), r#"["Reliable", 4, "Latest"]"#);
}

#[test]
fn topic_helpers_build_names_subscribe_and_publish() {
    let bus = fabric();
    let src = r#"
        let all = sys::confd::subscribe_changed();
        let some = sys::confd::subscribe_changed("sys/ui/#");
        let n = sys::confd::publish_changed("sys/ui/theme", #{ path: "sys/ui/theme" });
        sys::confd::publish_changed("other/x", #{ path: "other/x" });
        [sys::confd::CHANGED_PATTERN, sys::confd::changed_topic("a/b"), n,
         all.next(10).payload.path, all.next(10).payload.path,
         some.next(10).payload.path, some.next(10), some.filter]
    "#;
    assert_eq!(
        value(&run_msg(&bus, src)),
        r#"["system/confd/changed/#", "system/confd/changed/a/b", 2, "sys/ui/theme", "other/x", "sys/ui/theme", (), "system/confd/changed/sys/ui/#"]"#
    );
}

#[test]
fn methods_that_transfer_kernel_objects_are_not_generated() {
    let bus = fabric();
    let message = failure(&run_msg(&bus, "sys::display::create_surface(1, 2)"));
    assert!(message.contains("create_surface"), "{message}");
    let source = api::module("display").unwrap().source;
    assert!(source.contains("Not callable from a script"));
    assert!(!source.contains("fn create_surface("));
}

#[test]
fn the_modules_are_compiled_once_per_fabric() {
    let fabric = Rc::new(Fabric::new(fabric()));
    let mut builds = 0;
    let mut first: Option<rhai::Shared<Module>> = None;
    for _ in 0..3 {
        let mut engine = Engine::new();
        crate::msg::install_fabric(&mut engine, &fabric).unwrap();
        let sys = fabric
            .api_namespace(|| {
                builds += 1;
                Ok(rhai::Shared::new(Module::new()))
            })
            .unwrap();
        if let Some(first) = &first {
            assert!(rhai::Shared::ptr_eq(first, &sys));
        }
        first = Some(sys);
        let alive: bool = engine.eval("sys::echo::ping()").unwrap();
        assert!(alive);
    }
    assert_eq!(builds, 0, "installing compiled the modules; later engines reuse them");
}

#[test]
fn describe_names_the_generated_module() {
    let text = crate::msg::describe("os.lazy.confd.v1").unwrap();
    assert!(text.contains("Rhai module: sys::confd"), "{text}");
}

#[test]
fn a_generated_module_runs_in_the_calling_engines_msg() {
    // Compile the modules with one engine, then call them from another whose
    // `msg` is bound to its own owner: the subscription must be filed there.
    let bus = fabric();
    let fabric = Rc::new(Fabric::new(bus));
    let mut first = Engine::new();
    crate::msg::install_hosted(&mut first, &fabric, "first").unwrap();
    let mut second = Engine::new();
    crate::msg::install_hosted(&mut second, &fabric, "second").unwrap();
    second
        .run_with_scope(
            &mut Scope::new(),
            r#"sys::confd::on_changed(|e| ())"#,
        )
        .unwrap();
    assert!(fabric.active("second"));
    assert!(!fabric.active("first"));
    assert_eq!(fabric.release("second"), 1);
    assert!(!fabric.active("second"));
}
