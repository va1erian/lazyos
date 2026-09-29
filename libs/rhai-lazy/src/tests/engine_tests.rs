//! Engine construction: limits, print routing, sandbox, outcome classification.

use super::{failure, run, run_with, value};
use crate::mock::MockHost;
use crate::{Config, Limits, Outcome};

fn with_limits(limits: Limits) -> Config {
    Config {
        limits,
        ..Default::default()
    }
}

#[test]
fn print_goes_to_the_host_stdout_and_debug_to_stderr() {
    let (out, host) = run(MockHost::new(), r#"print("hi"); debug(42); 7"#);
    assert_eq!(value(&out), "7");
    assert_eq!(host.out(), "hi\n");
    assert!(
        host.stderr.borrow().contains("42"),
        "{}",
        host.stderr.borrow()
    );
}

#[test]
fn an_infinite_loop_is_stopped_by_max_operations() {
    let config = with_limits(Limits {
        max_operations: 5_000,
        ..Default::default()
    });
    let (out, _) = run_with(MockHost::new(), &config, "loop { }");
    assert!(
        failure(&out).to_lowercase().contains("too many operations"),
        "{out:?}"
    );
}

#[test]
fn default_limits_stop_a_runaway_loop_too() {
    let (out, _) = run(MockHost::new(), "let i = 0; while true { i += 1; }");
    assert!(
        failure(&out).to_lowercase().contains("too many operations"),
        "{out:?}"
    );
}

#[test]
fn unlimited_operations_can_be_selected() {
    let config = with_limits(Limits {
        max_operations: 0,
        ..Default::default()
    });
    let (out, _) = run_with(
        MockHost::new(),
        &config,
        "let s = 0; for i in 0..200000 { s += i; } s",
    );
    assert_eq!(value(&out), "19999900000");
}

#[test]
fn deep_recursion_hits_the_call_level_limit_not_the_stack() {
    let (out, _) = run(MockHost::new(), "fn f(n) { f(n + 1) } f(0)");
    assert!(failure(&out).contains("Stack overflow"), "{out:?}");
}

#[test]
fn expression_nesting_is_limited() {
    let deep = alloc::format!("{}1{}", "(".repeat(500), ")".repeat(500));
    let (out, _) = run(MockHost::new(), &deep);
    assert!(
        failure(&out).contains("too complex") || failure(&out).contains("exceeds"),
        "{out:?}"
    );
}

#[test]
fn string_array_and_map_sizes_are_limited() {
    let config = with_limits(Limits {
        max_string_size: 16,
        max_array_size: 4,
        max_map_size: 2,
        ..Default::default()
    });
    for script in [
        r#"let s = "0123456789"; s + s"#,
        "let a = []; for i in 0..10 { a.push(i); } a",
        "let m = #{}; m.a = 1; m.b = 2; m.c = 3; m",
    ] {
        let (out, _) = run_with(MockHost::new(), &config, script);
        assert!(
            failure(&out).contains("too large") || failure(&out).contains("exceed"),
            "{script}: {out:?}"
        );
    }
}

#[test]
fn eval_works_by_default_and_is_off_in_the_sandbox() {
    let (out, _) = run(MockHost::new(), r#"eval("1 + 2")"#);
    assert_eq!(value(&out), "3");

    let config = Config {
        sandbox: true,
        ..Default::default()
    };
    let (out, _) = run_with(MockHost::new(), &config, r#"eval("1 + 2")"#);
    assert!(!matches!(out, Outcome::Value(_)), "{out:?}");
}

#[test]
fn import_resolves_nothing_in_the_sandbox() {
    let config = Config {
        sandbox: true,
        ..Default::default()
    };
    let (out, _) = run_with(MockHost::new(), &config, r#"import "/etc/x.rhai" as x; 1"#);
    assert!(matches!(out, Outcome::Failed(_)), "{out:?}");
}

#[test]
fn a_closed_stdout_stops_the_script_quietly() {
    let host = MockHost::new();
    host.stdout_closed.set(true);
    let (out, _) = run(host, "loop { print(1); }");
    assert!(matches!(out, Outcome::OutputClosed), "{out:?}");
}

#[test]
fn syntax_errors_carry_a_position() {
    let (out, _) = run(MockHost::new(), "let x = ;");
    assert!(failure(&out).contains("line 1"), "{out:?}");
}

#[test]
fn runtime_errors_carry_a_position() {
    let (out, _) = run(MockHost::new(), "let a = 1;\nlet b = a + missing;");
    assert!(failure(&out).contains("line 2"), "{out:?}");
    let (out, _) = run(MockHost::new(), "let a = 1;\nread(\"/nope\");");
    assert!(failure(&out).contains("line 2"), "{out:?}");
}

#[test]
fn builtin_arithmetic_errors_have_no_position_upstream() {
    // Pinned so a Rhai upgrade that adds one is noticed and the docs updated.
    let (out, _) = run(MockHost::new(), "let a = 1;\nlet b = a / 0;");
    assert!(failure(&out).contains("Division by zero"), "{out:?}");
}

#[test]
fn a_shebang_line_is_ignored() {
    let (out, _) = run(MockHost::new(), "#!/bin/rhai\n40 + 2");
    assert_eq!(value(&out), "42");
    // Line numbers still match the file.
    let (out, _) = run(MockHost::new(), "#!/bin/rhai\nlet a = 1;\nmissing");
    assert!(failure(&out).contains("line 3"), "{out:?}");
}

#[test]
fn script_errors_thrown_with_throw_are_reported() {
    let (out, _) = run(MockHost::new(), r#"throw "boom""#);
    assert!(failure(&out).contains("boom"));
}
