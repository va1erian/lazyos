//! The snippet property tests: every generated text compiles under the
//! rhai-lazy engine and, over the in-memory fabric, runs without a codec
//! error. They iterate the whole table, so a new `.midl` that breaks the
//! generator fails here.

use rhai_lazy::msg::schema::{self, Ty};
use rhai_lazy::Outcome;

use super::zero_lab;
use crate::prelude;
use crate::rhai_names::{module_usable, scriptable};
use crate::snippet::{self, CallOptions, Output, SnippetError, Style, TopicAction};
use crate::testkit::Lab;

fn explain(outcome: &Outcome) -> String {
    format!("{outcome:?}")
}

/// A service refusing a call ("... (ENOENT, code 2)") is an answer, not a
/// codec error: the arguments were encoded and decoded fine.
fn is_service_refusal(outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::Failed(m) if m.contains(", code "))
}

fn expect_ran(lab: &Lab, label: &str, text: &str) {
    let ran = lab.run(text);
    assert!(
        matches!(ran.outcome, Outcome::Value(_)) || is_service_refusal(&ran.outcome),
        "{label}: {}\n--- snippet ---\n{text}",
        explain(&ran.outcome)
    );
}

#[test]
fn every_method_call_snippet_compiles_and_runs_without_a_codec_error() {
    let lab = zero_lab();
    let mut compiled = 0;
    let mut ran = 0;
    for interface in schema::interfaces() {
        for method in interface.methods {
            let label = format!("{}.{}", interface.name, method.name);
            let auto = snippet::call(interface, method, &CallOptions::default());
            if !method.objects.is_empty() {
                assert!(matches!(auto, Err(SnippetError::Objects(_))), "{label}");
                continue;
            }
            if !scriptable(interface, method) {
                assert_eq!(auto.unwrap_err(), SnippetError::GateOnly, "{label}");
                continue;
            }
            // Every style the method supports.
            let mut variants = vec![
                (Style::Auto, Output::Print),
                (Style::Connect, Output::Print),
            ];
            if module_usable(interface) {
                variants.push((Style::Sys, Output::None));
            }
            for (style, output) in variants {
                let options = CallOptions {
                    style,
                    output,
                    ..CallOptions::default()
                };
                let snippet = snippet::call(interface, method, &options)
                    .unwrap_or_else(|e| panic!("{label} {style:?}: {e}"));
                lab.compile(&snippet.text)
                    .unwrap_or_else(|e| panic!("{label} {style:?}: {e}\n{}", snippet.text));
                compiled += 1;
                expect_ran(&lab, &format!("{label} {style:?}"), &snippet.text);
                ran += 1;
            }
        }
    }
    assert!(compiled > 300, "compiled {compiled}");
    assert_eq!(compiled, ran);
}

#[test]
fn a_call_snippet_with_pg_show_runs_with_the_prelude() {
    let lab = zero_lab();
    let echo = schema::interface("os.lazy.echo.v1").unwrap();
    let options = CallOptions {
        output: Output::Show,
        ..CallOptions::default()
    };
    let snippet = snippet::call(echo, echo.method("Echo").unwrap(), &options).unwrap();
    assert!(snippet.text.contains("pg_show(reply);"));
    let text = format!("{}{}", prelude::SOURCE, snippet.text);
    let ran = lab.run(&text);
    ran.ok();
    assert_eq!(ran.lines, ["\"\""]);
    // A multi-value reply is a map tree.
    let timed = schema::interface("os.lazy.timed.v1").unwrap();
    let snippet = snippet::call(timed, timed.method("Now").unwrap(), &options).unwrap();
    let ran = lab.run(&format!("{}{}", prelude::SOURCE, snippet.text));
    ran.ok();
    assert_eq!(ran.lines[0], "map(4)");
    assert!(
        ran.lines.contains(&"  tz_name: \"\"".to_string()),
        "{:?}",
        ran.lines
    );
}

#[test]
fn call_text_reads_like_the_plan_shows() {
    let confd = schema::interface("os.lazy.confd.v1").unwrap();
    let get = snippet::call(confd, confd.method("Get").unwrap(), &CallOptions::default()).unwrap();
    assert_eq!(
        get.text,
        "let reply = sys::confd::get(/* path: String */ \"\");\nprint(reply);\n"
    );
    let connect = snippet::call(
        confd,
        confd.method("Get").unwrap(),
        &CallOptions {
            style: Style::Connect,
            ..CallOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        connect.text,
        "let svc = msg::connect(\"os.lazy.confd.v1\");\nlet reply = svc.get(/* path: String */ \"\");\nprint(reply);\n"
    );
    let notify = schema::interface("os.lazy.notify.v1").unwrap();
    let text = snippet::call(
        notify,
        notify.method("Notify").unwrap(),
        &CallOptions::default(),
    )
    .unwrap()
    .text;
    assert!(
        text.starts_with("let reply = sys::notify::notify(\n    \"Info\",  // level: Level\n"),
        "{text}"
    );
    assert!(text.contains("    (),  // icon: Option<Icon>\n"));
    assert!(text.contains("    []"));
}

#[test]
fn a_named_service_is_connected_by_its_registered_name() {
    let accounts = schema::interface("os.lazy.accounts.v1").unwrap();
    let call = snippet::call(
        accounts,
        accounts.method("Lookup").unwrap(),
        &CallOptions {
            style: Style::Connect,
            service: Some("os.lazy.accountsd"),
            ..CallOptions::default()
        },
    )
    .unwrap();
    assert!(call
        .text
        .starts_with("let svc = msg::connect(\"os.lazy.accounts.v1\", \"os.lazy.accountsd\");"));
    let default = snippet::connect(accounts, "os.lazy.accounts");
    assert!(default
        .text
        .starts_with("let svc = msg::connect(\"os.lazy.accounts.v1\");"));
    let named = snippet::connect(accounts, "demo.mine");
    assert!(named.text.contains("\"demo.mine\""));
}

#[test]
fn one_way_methods_use_invoke_oneway_in_connect_style() {
    let echo = schema::interface("os.lazy.echo.v1").unwrap();
    let notify = echo.method("Notify").unwrap();
    assert!(notify.oneway);
    let call = snippet::call(
        echo,
        notify,
        &CallOptions {
            style: Style::Connect,
            ..CallOptions::default()
        },
    )
    .unwrap();
    assert!(
        call.text.contains("svc.invoke_oneway(\"Notify\""),
        "{}",
        call.text
    );
    assert_eq!(call.notes.len(), 1);
}

#[test]
fn sys_style_is_refused_where_there_is_no_function() {
    let registry = schema::interface("os.lazy.messenger.registry.v1").unwrap();
    let connect = registry.method("Connect").unwrap();
    assert_eq!(
        snippet::call(registry, connect, &CallOptions::default()).unwrap_err(),
        SnippetError::GateOnly
    );
    let scope = schema::interface("os.lazy.messenger.topics.publish.v1").unwrap();
    let method = &scope.methods[0];
    let err = snippet::call(
        scope,
        method,
        &CallOptions {
            style: Style::Sys,
            ..CallOptions::default()
        },
    )
    .unwrap_err();
    assert_eq!(err, SnippetError::NoModule);
    // Auto falls back to connect for the same method.
    let auto = snippet::call(scope, method, &CallOptions::default()).unwrap();
    assert!(auto.text.contains("msg::connect"));
}

#[test]
fn placeholders_are_zero_values_with_enums_at_their_first_variant() {
    let notify = schema::interface("os.lazy.notify.v1").unwrap();
    assert_eq!(
        snippet::placeholder(notify, Ty::Struct("Icon")),
        "#{ lucide: (), image: () }"
    );
    assert_eq!(snippet::placeholder(notify, Ty::Enum("Level")), "\"Info\"");
    assert_eq!(snippet::placeholder(notify, Ty::Bytes), "blob()");
    assert_eq!(snippet::placeholder(notify, Ty::Array(&Ty::String)), "[]");
    assert_eq!(snippet::placeholder(notify, Ty::Option(&Ty::U32)), "()");
    assert_eq!(snippet::placeholder(notify, Ty::F64), "0.0");
}

#[test]
fn every_serve_skeleton_registers_and_names_only_real_methods() {
    let lab = zero_lab();
    let mut served = 0;
    for interface in schema::interfaces() {
        let snippet = snippet::serve(interface);
        lab.compile(&snippet.text)
            .unwrap_or_else(|e| panic!("{}: {e}\n{}", interface.name, snippet.text));
        // The last statement waits for requests; the registration is what
        // checks the handler names against the interface.
        let body = snippet
            .text
            .strip_suffix("msg::run();\n")
            .unwrap_or_else(|| panic!("{} does not end in msg::run()", interface.name));
        lab.bus.set_idle_budget(3);
        expect_ran(&lab, &format!("serve {}", interface.name), body);
        served += 1;
        assert_eq!(
            snippet.notes.len(),
            interface
                .methods
                .iter()
                .filter(|m| !m.objects.is_empty())
                .count()
        );
    }
    assert_eq!(served, schema::interfaces().len());
}

#[test]
fn a_served_skeleton_answers_with_zero_values() {
    use crate::testkit::bus::s;
    use std::cell::Cell;
    use std::rc::Rc;

    let lab = Lab::new();
    let echo = schema::interface("os.lazy.echo.v1").unwrap();
    let snippet = snippet::serve(echo);
    lab.bus.set_idle_budget(2);
    let txn = Rc::new(Cell::new(0));
    let seen = txn.clone();
    lab.bus.on_idle(move |bus| {
        let mut args = rhai::Map::new();
        args.insert("text".into(), s("hi"));
        args.insert("count".into(), rhai::Dynamic::from_int(2));
        seen.set(bus.inject_request("demo.echo", "Echo", args));
    });
    let ran = lab.run(&snippet.text);
    assert!(
        ran.failure().contains("harness: the script is idle"),
        "{:?}",
        ran.outcome
    );
    let reply = lab.bus.reply_to("demo.echo", "Echo", txn.get()).unwrap();
    assert_eq!(reply["reply"].clone().into_string().unwrap(), "");
}

#[test]
fn every_declared_topic_snippet_compiles_and_runs() {
    let lab = zero_lab();
    let mut topics = 0;
    for interface in schema::interfaces() {
        for topic in interface.topics {
            let label = format!("{} {}", interface.name, topic.pattern);
            for action in [
                TopicAction::On,
                TopicAction::OnMsg,
                TopicAction::Subscribe,
                TopicAction::Peek,
                TopicAction::Publish,
            ] {
                let snippet = snippet::topic(interface, topic, action);
                lab.compile(&snippet.text)
                    .unwrap_or_else(|e| panic!("{label} {action:?}: {e}\n{}", snippet.text));
                let body = snippet
                    .text
                    .strip_suffix("msg::run();\n")
                    .unwrap_or(&snippet.text);
                lab.bus.set_idle_budget(3);
                expect_ran(&lab, &format!("{label} {action:?}"), body);
            }
            topics += 1;
        }
    }
    assert!(topics > 15, "{topics}");
}

#[test]
fn a_published_snippet_reaches_a_subscriber_decoded_by_its_declared_type() {
    let lab = zero_lab();
    let timed = schema::interface("os.lazy.timed.v1").unwrap();
    let topic = &timed.topics[0];
    let sub = lab.run("let s = msg::subscribe(\"time/tick\"); 1");
    sub.ok();
    // The subscription above is gone with its scope; do it in one script.
    let publish = snippet::topic(timed, topic, TopicAction::Publish).text;
    let ran = lab.run(&format!(
        "let s = msg::subscribe(\"time/tick\");\n{publish}let e = s.next(100);\nprint(e.payload);\nprint(e.retained);"
    ));
    ran.ok();
    assert_eq!(
        ran.lines,
        ["#{\"offset\": 0, \"unix\": 0, \"zone_name\": \"\"}", "true"]
    );
}

#[test]
fn undeclared_topic_snippets_work_on_bytes() {
    let lab = zero_lab();
    for action in [
        TopicAction::On,
        TopicAction::Subscribe,
        TopicAction::Peek,
        TopicAction::Publish,
    ] {
        let snippet = snippet::raw_topic("demo/pg/raw", action);
        lab.compile(&snippet.text).unwrap();
        let body = snippet
            .text
            .strip_suffix("msg::run();\n")
            .unwrap_or(&snippet.text);
        lab.bus.set_idle_budget(3);
        expect_ran(&lab, "raw", body);
    }
}

#[test]
fn wildcard_topics_publish_to_a_concrete_stand_in() {
    let health = schema::interface("os.lazy.healthd.v1").unwrap();
    let topic = health
        .topics
        .iter()
        .find(|t| t.pattern.ends_with("/+"))
        .unwrap();
    let snippet = snippet::topic(health, topic, TopicAction::Publish);
    assert!(snippet.text.contains("\"system/health/x\""));
    assert!(snippet.notes[0].contains("Replace the x"));
}
