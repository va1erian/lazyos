//! The `os` module: happy paths and every documented failure.

use alloc::string::ToString;
use alloc::vec;

use super::{failure, run, value};
use crate::host::EntryKind;
use crate::mock::{entry, MockHost};
use crate::Outcome;

#[test]
fn args_are_the_script_arguments() {
    let mut host = MockHost::new();
    host.args = vec!["a".to_string(), "b c".to_string()];
    let (out, _) = run(host, "os::args()");
    assert_eq!(value(&out), r#"["a", "b c"]"#);
}

#[test]
fn env_reads_one_variable_or_the_whole_map() {
    let host = MockHost::new().with_env("HOME", "/root").with_env("A", "1");
    let (out, _) = run(host, r#"[env("HOME"), env("NOPE"), env().len()]"#);
    assert_eq!(value(&out), r#"["/root", (), 2]"#);
}

#[test]
fn functions_exist_as_globals_and_in_the_module() {
    let host = MockHost::new().with_env("K", "v");
    let (out, _) = run(host, r#"env("K") + os::env("K")"#);
    assert_eq!(value(&out), "vv");
}

#[test]
fn exit_carries_its_status_and_is_not_catchable() {
    let (out, _) = run(MockHost::new(), "exit(3)");
    assert!(matches!(out, Outcome::Exit(3)));
    let (out, _) = run(MockHost::new(), "try { exit(4) } catch { 9 }");
    assert!(matches!(out, Outcome::Exit(4)), "{out:?}");
    let (out, _) = run(MockHost::new(), "exit()");
    assert!(matches!(out, Outcome::Exit(0)));
}

#[test]
fn exit_status_is_reduced_to_a_byte() {
    let (out, _) = run(MockHost::new(), "exit(-1)");
    assert!(matches!(out, Outcome::Exit(255)));
    let (out, _) = run(MockHost::new(), "exit(256)");
    assert!(matches!(out, Outcome::Exit(0)));
}

#[test]
fn clock_reports_the_host_clock() {
    let host = MockHost::new();
    host.clock.set(12.5);
    let (out, _) = run(host, "clock()");
    assert_eq!(value(&out), "12.5");
}

#[test]
fn sleep_calls_the_host_and_rejects_bad_durations() {
    let (out, host) = run(MockHost::new(), "sleep(250)");
    assert!(matches!(out, Outcome::Value(_)));
    assert_eq!(*host.slept.borrow(), vec![250]);

    let (out, host) = run(MockHost::new(), "sleep(-1)");
    assert!(failure(&out).contains("negative duration"));
    assert!(host.slept.borrow().is_empty());

    let (out, host) = run(MockHost::new(), "sleep(3600001)");
    assert!(failure(&out).contains("limit"), "{out:?}");
    assert!(host.slept.borrow().is_empty());
}

#[test]
fn read_returns_file_text() {
    let host = MockHost::new().with_file("/etc/motd", b"hello\n");
    let (out, _) = run(host, r#"read("/etc/motd")"#);
    assert_eq!(value(&out), "hello\n");
}

#[test]
fn read_failures_are_catchable_errors() {
    let host = MockHost::new()
        .with_file("/bad", &[0xff, 0xfe, 0x00])
        .with_file("/secret", b"x")
        .with_file("/big", &[b'a'; 64]);
    host.deny("/secret");
    let script = r#"
        let msgs = [];
        for p in ["/missing", "/bad", "/secret"] {
            try { read(p); msgs.push("no error"); } catch (e) { msgs.push(e); }
        }
        msgs
    "#;
    let (out, _) = run(host, script);
    let text = value(&out);
    assert!(text.contains("os::read: /missing: No such file"), "{text}");
    assert!(text.contains("os::read: /bad: not valid UTF-8"), "{text}");
    assert!(
        text.contains("os::read: /secret: Permission denied"),
        "{text}"
    );
    assert!(!text.contains("no error"), "{text}");
}

#[test]
fn read_respects_the_size_cap() {
    let host = MockHost::new().with_file("/big", &[b'a'; 64]);
    let config = crate::Config {
        limits: crate::Limits {
            max_io_bytes: 16,
            ..Default::default()
        },
        ..Default::default()
    };
    let (out, _) = super::run_with(host, &config, r#"read("/big")"#);
    assert!(failure(&out).contains("larger than"), "{out:?}");
}

#[test]
fn write_then_read_round_trips_and_reports_denial() {
    let host = MockHost::new();
    host.deny("/ro");
    let (out, host) = run(
        host,
        r#"write("/tmp/a", "data"); let r = read("/tmp/a"); let m = ""; try { write("/ro", "x") } catch (e) { m = e; } r + "|" + m"#,
    );
    let text = value(&out);
    assert!(
        text.starts_with("data|os::write: /ro: Permission denied"),
        "{text}"
    );
    assert_eq!(host.files.borrow()["/tmp/a"], b"data");
}

#[test]
fn ls_lists_sorted_entries_with_name_size_kind() {
    let host = MockHost::new();
    host.dirs.borrow_mut().insert(
        "/d".to_string(),
        vec![
            entry("b.txt", 7, EntryKind::File),
            entry("a", 0, EntryKind::Dir),
            entry("l", 3, EntryKind::Symlink),
        ],
    );
    let (out, _) = run(
        host,
        r#"ls("/d").map(|e| e.name + ":" + e.size + ":" + e.kind)"#,
    );
    assert_eq!(value(&out), r#"["a:0:dir", "b.txt:7:file", "l:3:symlink"]"#);
}

#[test]
fn ls_failures_are_catchable_errors() {
    let host = MockHost::new();
    host.deny("/locked");
    host.dirs.borrow_mut().insert("/locked".to_string(), vec![]);
    let (out, _) = run(
        host,
        r#"let m = []; for p in ["/nope", "/locked"] { try { ls(p) } catch (e) { m.push(e) } } m"#,
    );
    let text = value(&out);
    assert!(text.contains("os::ls: /nope: No such file"), "{text}");
    assert!(
        text.contains("os::ls: /locked: Permission denied"),
        "{text}"
    );
}

#[test]
fn ls_enforces_the_entry_cap() {
    let host = MockHost::new();
    let many = (0..20)
        .map(|i| entry(&alloc::format!("f{i}"), 0, EntryKind::File))
        .collect();
    host.dirs.borrow_mut().insert("/many".to_string(), many);
    let config = crate::Config {
        limits: crate::Limits {
            max_array_size: 10,
            ..Default::default()
        },
        ..Default::default()
    };
    let (out, _) = super::run_with(host, &config, r#"ls("/many")"#);
    assert!(failure(&out).contains("too many"), "{out:?}");
}

#[test]
fn stdin_text_reads_input_and_rejects_bad_utf8() {
    let (out, _) = run(
        MockHost::new().with_stdin(b"line1\nline2\n"),
        "stdin_text()",
    );
    assert_eq!(value(&out), "line1\nline2\n");
    let (out, _) = run(MockHost::new().with_stdin(&[0xc3, 0x28]), "stdin_text()");
    assert!(failure(&out).contains("not valid UTF-8"), "{out:?}");
}

#[test]
fn a_second_stdin_read_sees_end_of_input() {
    let (out, _) = run(
        MockHost::new().with_stdin(b"once"),
        "let a = stdin_text(); let b = stdin_text(); a + \"|\" + b",
    );
    assert_eq!(value(&out), "once|");
}

#[test]
fn stdin_over_the_cap_is_an_error() {
    let config = crate::Config {
        limits: crate::Limits {
            max_io_bytes: 4,
            ..Default::default()
        },
        ..Default::default()
    };
    let (out, _) = super::run_with(
        MockHost::new().with_stdin(b"0123456789"),
        &config,
        "stdin_text()",
    );
    assert!(failure(&out).contains("larger than"), "{out:?}");
}

#[test]
fn host_functions_are_never_constant_folded() {
    // Compile first, change the world, then evaluate: a folded call would
    // still hold the value the environment had at compile time.
    use crate::{build_engine, Config, Scope};
    let host = alloc::rc::Rc::new(MockHost::new().with_env("K", "before"));
    let engine = build_engine(host.clone(), &Config::default());
    let ast = engine.compile(r#"env("K")"#).unwrap();
    host.env.borrow_mut().insert("K".into(), "after".into());
    let result: alloc::string::String =
        engine.eval_ast_with_scope(&mut Scope::new(), &ast).unwrap();
    assert_eq!(result, "after");
}
