//! The REPL: continuation detection, persistence, commands, errors.

use alloc::rc::Rc;
use alloc::string::{String, ToString};

use crate::mock::MockHost;
use crate::repl::{is_incomplete, Repl, Step};
use crate::{build_engine, Config, Engine};

fn engine() -> Engine {
    build_engine(Rc::new(MockHost::new()), &Config::default())
}

/// Feed lines, returning the last step.
fn feed_all(repl: &mut Repl, lines: &[&str]) -> Step {
    let mut last = Step::Quiet;
    for line in lines {
        last = repl.feed(line);
    }
    last
}

fn show(text: &str) -> Step {
    Step::Show(text.to_string())
}

#[test]
fn a_complete_line_is_evaluated_and_printed() {
    let engine = engine();
    let mut repl = Repl::new(&engine);
    assert_eq!(repl.feed("1 + 2"), show("3"));
    assert_eq!(repl.feed(r#""s""#), show("\"s\""));
    assert_eq!(repl.feed("let x = 1;"), Step::Quiet);
    assert_eq!(repl.feed(""), Step::Quiet);
}

#[test]
fn variables_and_functions_persist() {
    let engine = engine();
    let mut repl = Repl::new(&engine);
    assert_eq!(repl.feed("let x = 40;"), Step::Quiet);
    assert_eq!(repl.feed("fn add(a, b) { a + b }"), Step::Quiet);
    assert_eq!(repl.feed("add(x, 2)"), show("42"));
    assert_eq!(repl.feed("x += 1; x"), show("41"));
}

#[test]
fn unfinished_input_asks_for_more() {
    // Each of these is complete only after the last line.
    let cases: &[&[&str]] = &[
        &["if true {", "  1", "} else {", "  2", "}"],
        &["let a = [1, 2,", "3];", "a.len()"],
        &["fn f() {", "  10", "}", "f()"],
        &["1 +", "2"],
        &["foo(", "1)"],
        &["let s = `abc", "def`;", "s.len()"],
        &["#{", "a: 1", "}.a"],
    ];
    for lines in cases {
        let engine = engine();
        let mut repl = Repl::new(&engine);
        for line in &lines[..lines.len() - 1] {
            let step = repl.feed(line);
            assert!(
                matches!(step, Step::More | Step::Quiet | Step::Show(_)),
                "{lines:?}: {step:?}"
            );
        }
        // The full text must at least not be reported as an error before the
        // final line completes it.
        let mut repl = Repl::new(&engine);
        for line in &lines[..1] {
            assert_eq!(
                repl.feed(line),
                Step::More,
                "{lines:?}: first line must continue"
            );
        }
    }
}

#[test]
fn a_multi_line_block_runs_when_it_closes() {
    let engine = engine();
    let mut repl = Repl::new(&engine);
    assert_eq!(repl.feed("fn sq(n) {"), Step::More);
    assert!(repl.is_continuing());
    assert_eq!(repl.prompt(), "  ... ");
    assert_eq!(repl.feed("  n * n"), Step::More);
    assert_eq!(repl.feed("}"), Step::Quiet);
    assert_eq!(repl.prompt(), "rhai> ");
    assert_eq!(repl.feed("sq(7)"), show("49"));
}

#[test]
fn multi_line_strings_and_arrays_continue() {
    let engine = engine();
    let mut repl = Repl::new(&engine);
    assert_eq!(repl.feed("let s = `one"), Step::More);
    assert_eq!(repl.feed("two`;"), Step::Quiet);
    assert_eq!(repl.feed("s.len()"), show("7"));
    assert_eq!(repl.feed("[1,"), Step::More);
    assert_eq!(repl.feed("2]"), show("[1, 2]"));
}

#[test]
fn a_real_syntax_error_is_reported_immediately_not_continued() {
    let engine = engine();
    let mut repl = Repl::new(&engine);
    for bad in ["1 2", "let = 3;", "1 + * 2", "\"unterminated", ")"] {
        match repl.feed(bad) {
            Step::Error(message) => assert!(message.contains("line 1"), "{bad}: {message}"),
            other => panic!("{bad}: expected an error, got {other:?}"),
        }
        assert!(!repl.is_continuing(), "{bad} left the REPL waiting");
    }
}

#[test]
fn incompleteness_is_read_from_the_parser() {
    let engine = engine();
    let check = |text: &str| engine.compile(text).err().map(|e| is_incomplete(&e, text));
    assert_eq!(check("if x {"), Some(true));
    assert_eq!(check("let a = [1,"), Some(true));
    assert_eq!(check("f(1,"), Some(true));
    assert_eq!(check("1 +"), Some(true));
    assert_eq!(check("`abc"), Some(true));
    assert_eq!(check("let s = `abc"), Some(true));
    assert_eq!(check("if x {\n  1\n"), Some(true));
    assert_eq!(check("1 2"), Some(false));
    assert_eq!(check("let = 3"), Some(false));
    assert_eq!(check("\"abc"), Some(false));
}

#[test]
fn runtime_errors_show_position_and_keep_the_session() {
    let engine = engine();
    let mut repl = Repl::new(&engine);
    repl.feed("let x = 5;");
    match repl.feed("x + missing") {
        Step::Error(message) => assert!(message.contains("line 1"), "{message}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(repl.feed("x"), show("5"));
}

#[test]
fn a_failed_multi_line_entry_does_not_poison_the_next() {
    let engine = engine();
    let mut repl = Repl::new(&engine);
    assert_eq!(repl.feed("{"), Step::More);
    assert!(matches!(repl.feed("1 2 }"), Step::Error(_)));
    assert!(!repl.is_continuing());
    assert_eq!(repl.feed("6 * 7"), show("42"));
}

#[test]
fn cancel_drops_an_unfinished_entry() {
    let engine = engine();
    let mut repl = Repl::new(&engine);
    assert_eq!(repl.feed("if true {"), Step::More);
    assert_eq!(repl.feed(":cancel"), Step::Quiet);
    assert!(!repl.is_continuing());
    assert_eq!(repl.feed("1"), show("1"));
}

#[test]
fn history_records_entries_and_is_bounded() {
    let engine = engine();
    let mut repl = Repl::new(&engine);
    repl.feed("let a = 1;");
    repl.feed("fn f() {");
    repl.feed("  a");
    repl.feed("}");
    assert_eq!(repl.history(), ["let a = 1;", "fn f() {\n  a\n}"]);
    match repl.feed(":history") {
        Step::Show(text) => {
            assert!(text.contains("1  let a = 1;"), "{text}");
            assert!(text.contains("2  fn f() {"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    for i in 0..1100 {
        repl.feed(&alloc::format!("{i}"));
    }
    assert_eq!(repl.history().len(), 1000);
    assert_eq!(repl.history().last().map(String::as_str), Some("1099"));
}

#[test]
fn commands_help_reset_quit_and_unknown() {
    let engine = engine();
    let mut repl = Repl::new(&engine);
    assert!(matches!(repl.feed(":help"), Step::Show(text) if text.contains(":quit")));
    repl.feed("let x = 1;");
    assert_eq!(repl.feed(":reset"), Step::Quiet);
    assert!(matches!(repl.feed("x"), Step::Error(_)));
    assert!(matches!(repl.feed(":bogus"), Step::Error(text) if text.contains(":bogus")));
    assert_eq!(repl.feed(":quit"), Step::Exit(0));
}

#[test]
fn exit_inside_the_repl_leaves_with_its_status() {
    let engine = engine();
    let mut repl = Repl::new(&engine);
    assert_eq!(repl.feed("exit(5)"), Step::Exit(5));
}

#[test]
fn an_oversized_pending_entry_is_discarded() {
    let engine = engine();
    let mut repl = Repl::new(&engine);
    assert_eq!(repl.feed("let s = `"), Step::More);
    let chunk = "x".repeat(64 * 1024);
    let mut result = Step::More;
    for _ in 0..20 {
        result = repl.feed(&chunk);
        if matches!(result, Step::Error(_)) {
            break;
        }
    }
    assert!(matches!(result, Step::Error(_)), "{result:?}");
    assert!(!repl.is_continuing());
    assert_eq!(repl.feed("1"), show("1"));
}

#[test]
fn print_output_goes_to_the_host_not_the_step() {
    let host = Rc::new(MockHost::new());
    let engine = build_engine(host.clone(), &Config::default());
    let mut repl = Repl::new(&engine);
    assert_eq!(repl.feed(r#"print("out")"#), Step::Quiet);
    assert_eq!(host.out(), "out\n");
}

#[test]
fn many_entries_do_not_grow_state_without_bound() {
    // Soak: the function AST must not accumulate one copy per entry.
    let engine = engine();
    let mut repl = Repl::new(&engine);
    for i in 0..3000 {
        assert!(matches!(
            repl.feed(&alloc::format!("{i} + 1")),
            Step::Show(_)
        ));
    }
    assert_eq!(repl.feed("fn k() { 1 }"), Step::Quiet);
    for _ in 0..500 {
        assert_eq!(repl.feed("k()"), show("1"));
    }
}

#[test]
fn bang_recalls_history_entries() {
    let engine = engine();
    let mut repl = Repl::new(&engine);
    assert_eq!(repl.feed("let n = 1;"), Step::Quiet);
    assert_eq!(repl.feed("n += 1; n"), show("2"));
    assert_eq!(repl.feed("!!"), show("3"));
    assert_eq!(repl.feed("!2"), show("4"));
    assert_eq!(
        repl.feed("!1"),
        Step::Quiet,
        "re-running `let n = 1;` resets n"
    );
    assert_eq!(repl.feed("n"), show("1"));
    for bad in ["!0", "!99", "!99999999999999999999999"] {
        assert!(
            matches!(repl.feed(bad), Step::Error(m) if m.contains("no such history")),
            "{bad}"
        );
    }
    // Rhai's own `!` operator is untouched.
    assert_eq!(repl.feed("!true"), show("false"));
    assert_eq!(repl.feed("!!true"), show("true"));
}

#[test]
fn bang_recalls_multi_line_entries_whole() {
    let engine = engine();
    let mut repl = Repl::new(&engine);
    repl.feed("fn f() {");
    repl.feed("  7");
    repl.feed("}");
    repl.feed("f()");
    assert_eq!(repl.feed("!1"), Step::Quiet);
    assert_eq!(repl.feed("!2"), show("7"));
}

#[test]
fn interrupt_drops_an_unfinished_entry() {
    let engine = engine();
    let mut repl = Repl::new(&engine);
    assert_eq!(repl.feed("if true {"), Step::More);
    repl.interrupt();
    assert!(!repl.is_continuing());
    assert_eq!(repl.feed("1"), show("1"));
}

#[test]
fn feed_all_helper_reports_the_last_step() {
    let engine = engine();
    let mut repl = Repl::new(&engine);
    assert_eq!(feed_all(&mut repl, &["let a = 2;", "a * a"]), show("4"));
}
