use super::zero_lab;
use crate::prelude;
use crate::run_map::{compose, compose_auto};
use crate::runner::{RunId, RunState, RunTable, Stream, MAX_FINISHED, MAX_LIVE, MAX_OUTPUT_LINES};
use crate::runner_fake::{FakeRunner, Plan};
use crate::util::fnv1a64;

#[test]
fn compose_adds_the_prelude_only_when_asked_for_or_used() {
    let plain = compose("print(1);\n", false, 1);
    assert_eq!((plain.text.as_str(), plain.offset), ("print(1);\n", 0));
    let with = compose("print(1);\n", true, 1);
    assert!(with.text.starts_with(prelude::SOURCE));
    assert!(with.text.ends_with("print(1);\n"));
    assert_eq!(with.offset, prelude::lines());
    assert_eq!(compose_auto("pg_show(1);", 1).offset, prelude::lines());
    assert_eq!(compose_auto("print(1);", 1).offset, 0);
}

#[test]
fn a_shebang_is_blanked_without_moving_a_line() {
    let c = compose("#!/system/bin/rhai\nlet x = ;\n", false, 1);
    assert_eq!(c.text, "\nlet x = ;\n");
    let c = compose("#!/system/bin/rhai\nlet x = ;\n", true, 1);
    let lab = zero_lab();
    let ran = lab.run(&c.text);
    let mark = c.map_error(ran.failure()).unwrap();
    assert_eq!(mark.line, Some(2));
}

/// The error text the engine gives for `script` run behind the prelude.
fn error_of(script: &str) -> (crate::run_map::Composed, String) {
    let c = compose(script, true, 1);
    let lab = zero_lab();
    let ran = lab.run(&c.text);
    (c, ran.failure().to_string())
}

fn lines_of(script: &str) -> Vec<Option<usize>> {
    let (c, message) = error_of(script);
    message
        .lines()
        .filter_map(|l| c.map_error(l))
        .map(|m| m.line)
        .collect()
}

#[test]
fn error_positions_from_the_engine_map_back_to_editor_lines() {
    // A syntax error on line 2.
    assert_eq!(lines_of("let x = 1;\nlet y = ;\n"), [Some(2)]);
    // A throw on line 2.
    assert_eq!(lines_of("let x = 1;\nthrow \"boom\";\n"), [Some(2)]);
    // A throw inside a function: the throw line, then the call line.
    assert_eq!(
        lines_of("let x = 1;\nfn f() { throw \"no\"; }\nf();\n"),
        [Some(2), Some(3)]
    );
    // A missing method inside a function called from line 4.
    assert_eq!(
        lines_of("let x = 1;\nfn f(a) { a.nope(); }\nlet z = 2;\nf(1);\n"),
        [Some(2), Some(4)]
    );
    // An unknown function.
    assert_eq!(lines_of("\n\nunknown_fn(3);\n"), [Some(3)]);
    // Runaway loops stop with a position too.
    assert_eq!(lines_of("let n = 0;\nloop { n += 1; }\n"), [Some(2)]);
}

#[test]
fn a_position_inside_a_generated_module_is_not_the_users_line() {
    // `@ 'sys' (line 28, ...)` is the generated module's source; the call
    // site is on the next line of the message.
    let (c, message) = error_of("let x = 1;\nsys::confd::get(5);\n");
    assert!(message.contains("@ 'sys'"), "{message}");
    let marks: Vec<_> = message.lines().filter_map(|l| c.map_error(l)).collect();
    assert_eq!(marks.len(), 1, "{message}");
    assert_eq!(marks[0].line, Some(2));
    assert!(marks[0].text.starts_with("in call to function 'get'"));
}

#[test]
fn an_error_inside_the_prelude_has_no_editor_line() {
    let (c, message) = error_of("pg_blob(5);\n");
    let marks: Vec<_> = message.lines().filter_map(|l| c.map_error(l)).collect();
    assert!(marks.iter().any(|m| m.in_prelude()), "{message}");
    assert!(marks.iter().any(|m| m.line == Some(1)), "{message}");
}

#[test]
fn errors_without_a_position_have_no_mark() {
    let (c, message) = error_of("msg::connect(\"nope.v1\");\n");
    assert!(c.map_error(&message).is_none(), "{message}");
    let (c, message) = error_of("let x = 1;\nlet y = x / 0;\n");
    assert!(c.map_error(&message).is_none(), "{message}");
    assert!(c.map_error("").is_none());
    assert!(c.map_error("(line x, position y) (line 4)").is_none());
}

#[test]
fn a_selection_maps_to_the_lines_it_was_taken_from() {
    let selection = "let a = 1;\nthrow \"x\";\n";
    let c = compose(selection, true, 10);
    let lab = zero_lab();
    let ran = lab.run(&c.text);
    let mark = c.map_error(ran.failure()).unwrap();
    assert_eq!(mark.line, Some(11), "the selection started on line 10");
    let plain = compose(selection, false, 7);
    let ran = lab.run(&plain.text);
    assert_eq!(plain.map_error(ran.failure()).unwrap().line, Some(8));
}

fn table(plans: Vec<Plan>) -> RunTable<FakeRunner> {
    RunTable::new(FakeRunner::with_plans(plans))
}

#[test]
fn several_runs_live_at_once_with_their_own_output_and_status() {
    let mut t = table(vec![
        Plan::lines(&["a1", "a2"], 0),
        Plan::forever(&["server up"]),
        Plan::lines(&["c1"], 3),
    ]);
    let a = t.start("print(1);", "one.rhai", 1, 100).unwrap();
    let b = t.start("msg::run();", "serve.rhai", 1, 110).unwrap();
    let c = t.start("exit(3);", "three.rhai", 1, 120).unwrap();
    assert_eq!((a, b, c), (RunId(1), RunId(2), RunId(3)));
    assert_eq!(t.live_count(), 3);
    t.poll(200);
    assert_eq!(t.get(a).unwrap().state, RunState::Exited(0));
    assert_eq!(t.get(c).unwrap().state, RunState::Exited(3));
    assert_eq!(t.get(b).unwrap().state, RunState::Running);
    let texts = |id| -> Vec<String> {
        t.get(id)
            .unwrap()
            .output
            .iter()
            .map(|l| l.text.clone())
            .collect()
    };
    assert_eq!(texts(a), ["a1", "a2"]);
    assert_eq!(texts(b), ["server up"]);
    assert_eq!(t.live_count(), 1);
    assert_eq!(t.get(a).unwrap().elapsed_ms(999), 100);
    assert_eq!(t.get(b).unwrap().elapsed_ms(999), 889);
    assert_eq!(t.get(a).unwrap().lines_total, 2);
}

#[test]
fn stop_ends_one_run_and_stop_all_ends_the_rest() {
    let mut t = table(vec![
        Plan::forever(&[]),
        Plan::forever(&[]),
        Plan::forever(&[]),
    ]);
    let a = t.start("msg::run();", "a", 1, 0).unwrap();
    let b = t.start("msg::run();", "b", 1, 0).unwrap();
    let c = t.start("msg::run();", "c", 1, 0).unwrap();
    t.poll(10);
    t.stop(b);
    assert_eq!(t.get(b).unwrap().state, RunState::Stopping);
    t.stop(b);
    assert_eq!(t.runner().stopped, [b], "a second Stop asks nothing more");
    t.poll(20);
    assert_eq!(t.get(b).unwrap().state, RunState::Stopped);
    assert_eq!(t.get(a).unwrap().state, RunState::Running);
    t.stop_all();
    t.poll(30);
    for id in [a, c] {
        assert_eq!(t.get(id).unwrap().state, RunState::Stopped);
    }
    assert_eq!(t.live_count(), 0);
    assert_eq!(t.runner().alive(), 0);
}

#[test]
fn the_marker_carries_id_status_and_a_hash_never_the_script() {
    let script = "let secret_looking = \"hunter2\";\nprint(secret_looking);\n";
    let mut t = table(vec![Plan::lines(&["hunter2"], 0), Plan::error(&["boom"])]);
    let a = t.start(script, "a", 1, 0).unwrap();
    t.start("throw 1;", "b", 1, 0).unwrap();
    t.poll(5);
    let markers: Vec<String> = t.take_markers().iter().map(|m| m.args()).collect();
    let hash = fnv1a64(script.as_bytes());
    assert_eq!(markers[0], format!("1:START:{hash:016x}"));
    assert!(markers.contains(&format!("1:EXIT:0:{hash:016x}")));
    assert!(markers.iter().any(|m| m.starts_with("2:EXIT:1:")));
    assert_eq!(markers.len(), 4);
    for m in &markers {
        assert!(!m.contains("hunter2") && !m.contains("print"), "{m}");
    }
    assert!(t.take_markers().is_empty(), "taken once");
    assert_eq!(t.get(a).unwrap().hash, hash);

    let mut t = table(vec![Plan::forever(&[])]);
    let a = t.start("msg::run();", "a", 1, 0).unwrap();
    t.stop(a);
    t.poll(1);
    let last = t.take_markers().pop().unwrap();
    assert!(
        last.args().starts_with("1:EXIT:stopped:"),
        "{}",
        last.args()
    );
}

#[test]
fn a_child_that_cannot_start_is_a_failed_run_with_markers() {
    let mut t = table(vec![Plan {
        refuse: Some("no such file".into()),
        ..Plan::default()
    }]);
    let err = t.start("print(1);", "a", 1, 0).unwrap_err();
    assert_eq!(err, "no such file");
    let run = t.get(RunId(1)).unwrap();
    assert_eq!(run.state, RunState::Failed("no such file".into()));
    assert_eq!(run.state.status_text().as_deref(), Some("failed"));
    let markers: Vec<String> = t.take_markers().iter().map(|m| m.args()).collect();
    assert!(markers[1].starts_with("1:EXIT:failed:"));
}

#[test]
fn stderr_lines_carry_marks_and_stdout_lines_do_not() {
    let script = "let x = 1;\nfn f() { throw \"no\"; }\nf();\n";
    // Run without the prelude, as the table will: the lines are the user's own.
    let c = compose_auto(script, 1);
    let message = zero_lab().run(&c.text).failure().to_string();
    let lines: Vec<&str> = message.lines().collect();
    let mut t = table(vec![Plan {
        output: [(Stream::Stdout, "(line 3, position 1)".to_string())]
            .into_iter()
            .chain(lines.iter().map(|l| (Stream::Stderr, l.to_string())))
            .collect(),
        exit: Some(1),
        ..Plan::default()
    }]);
    let id = t.start(script, "e.rhai", 1, 0).unwrap();
    assert_eq!(t.runner().spawned[0].source, compose_auto(script, 1).text);
    assert_eq!(c.offset, 0);
    t.poll(1);
    let run = t.get(id).unwrap();
    assert!(run.output[0].mark.is_none(), "stdout is not an error");
    let marks: Vec<Option<usize>> = run.marks().map(|m| m.line).collect();
    assert_eq!(marks, [Some(2), Some(3)]);
}

#[test]
fn output_is_capped_per_run_and_the_counters_keep_counting() {
    let many: Vec<String> = (0..MAX_OUTPUT_LINES + 50)
        .map(|i| format!("line {i}"))
        .collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let mut t = table(vec![Plan::lines(&refs, 0)]);
    let id = t.start("x", "a", 1, 0).unwrap();
    t.poll(1);
    let run = t.get(id).unwrap();
    assert_eq!(run.output.len(), MAX_OUTPUT_LINES);
    assert_eq!(run.dropped_lines, 50);
    assert_eq!(run.lines_total as usize, MAX_OUTPUT_LINES + 50);
    assert_eq!(run.output[0].text, "line 50");
}

#[test]
fn old_finished_runs_are_forgotten_but_live_ones_never() {
    let mut t = table(
        (0..MAX_FINISHED + 10)
            .map(|_| Plan::lines(&[], 0))
            .chain(std::iter::once(Plan::forever(&[])))
            .collect(),
    );
    let first = t.start("a", "a", 1, 0).unwrap();
    let server = {
        for _ in 1..MAX_FINISHED + 10 {
            t.poll(0);
            t.start("a", "a", 1, 0).unwrap();
        }
        t.poll(0);
        t.start("msg::run();", "srv", 1, 0).unwrap()
    };
    t.poll(1);
    assert!(t.get(first).is_none(), "the oldest finished run is gone");
    assert!(t.get(server).is_some());
    assert!(t.runs().iter().filter(|r| !r.state.is_live()).count() <= MAX_FINISHED);
}

#[test]
fn the_table_refuses_a_run_past_the_live_cap_and_takes_it_again_after_a_stop() {
    let mut t = table((0..MAX_LIVE + 2).map(|_| Plan::forever(&[])).collect());
    let ids: Vec<RunId> = (0..MAX_LIVE)
        .map(|_| t.start("msg::run();", "srv", 1, 0).unwrap())
        .collect();
    assert_eq!(t.live_count(), MAX_LIVE);
    t.take_markers();
    let err = t.start("msg::run();", "one more", 1, 0).unwrap_err();
    assert!(err.contains("already live"), "{err}");
    assert_eq!(t.runs().len(), MAX_LIVE, "a refusal leaves no run behind");
    assert!(t.take_markers().is_empty(), "and no marker");
    assert_eq!(t.runner().spawned.len(), MAX_LIVE, "and no child");
    t.stop(ids[0]);
    t.poll(1);
    assert!(t.start("msg::run();", "again", 1, 2).is_ok());
}

#[test]
fn a_hostile_line_number_in_error_output_cannot_overflow() {
    // A script prints whatever it likes to its error output.
    let max = usize::MAX;
    for first_line in [1, 2, 40, max] {
        let c = compose(
            "let x = 1;
",
            false,
            first_line,
        );
        for text in [
            format!("boom (line {max}, position 1)"),
            format!("boom (line {}, position 1)", max - 1),
            format!("boom (line {}, position 18446744073709551615)", max / 2),
        ] {
            let mark = c.map_error(&text).expect("a position");
            assert!(mark.line.is_some_and(|l| l >= 1), "{text}");
        }
    }
}
