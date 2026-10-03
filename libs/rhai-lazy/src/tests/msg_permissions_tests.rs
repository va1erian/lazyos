//! `msg::permissions::derive`: the manifest entries a script needs.

use alloc::vec::Vec;

use crate::msg::permissions::{derive, Derived};

fn of(script: &str) -> Derived {
    derive([script])
}

#[test]
fn generated_calls_need_their_interface() {
    let found = of(r#"
        fn form_load() {
            theme_label.text = sys::confd::get("sys/ui/theme").str_value;
            let t = sys::timed::connect();
        }
    "#);
    assert_eq!(found.interfaces, ["os.lazy.confd.v1", "os.lazy.timed.v1"]);
    assert!(found.topics.is_empty());
}

#[test]
fn topic_helpers_need_their_declared_filter_not_the_interface() {
    let found = of(r#"
        sys::confd::on_changed("sys/ui/#", |e| ());
        let s = sys::healthd::subscribe_summary();
        sys::confd::publish_changed("sys/x", sys::confd::new_change());
        let name = sys::confd::changed_topic("sys/y");
        let all = sys::confd::CHANGED_PATTERN;
    "#);
    assert!(found.interfaces.is_empty(), "{:?}", found.interfaces);
    assert_eq!(
        found.topics,
        [
            "publish:system/confd/changed/#",
            "subscribe:system/confd/changed/#",
            "subscribe:system/health/summary",
        ]
    );
}

#[test]
fn literal_msg_calls_are_declared() {
    let found = of(r#"
        let c = msg::connect("os.lazy.keyd.v1");
        msg::on("app/user.me.todo/+/saved", |e| ());
        msg::subscribe(`time/tick`);
        msg::publish("app/user.me.todo/x", "hi");
        msg::connect("os.lazy.nonexistent.v1");
    "#);
    assert_eq!(found.interfaces, ["os.lazy.keyd.v1"]);
    assert_eq!(
        found.topics,
        [
            "publish:app/user.me.todo/x",
            "subscribe:app/user.me.todo/+/saved",
            "subscribe:time/tick",
        ]
    );
}

#[test]
fn comments_strings_and_runtime_names_are_not_calls() {
    let found = of(r#"
        // sys::confd::get("x")
        /* msg::connect("os.lazy.keyd.v1") */
        let text = "sys::timed::now()";
        let topic = "time/" + "tick";
        msg::subscribe(topic);
        msg::on(`app/${who}/x`, |e| ());
        msg::publish("Bad/Topic", "x");
    "#);
    assert_eq!(found, Derived::default());
}

#[test]
fn several_scripts_are_merged_sorted_and_unique() {
    let scripts: Vec<&str> = alloc::vec![
        "sys::confd::get(\"a\");",
        "sys::confd::list(\"b\"); sys::echo::ping();",
    ];
    let found = derive(scripts);
    assert_eq!(found.interfaces, ["os.lazy.confd.v1", "os.lazy.echo.v1"]);
}
