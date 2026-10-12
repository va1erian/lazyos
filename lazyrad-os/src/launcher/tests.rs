use std::collections::VecDeque;
use std::io;

use super::*;

/// A reader that plays back scripted results.
struct Script(VecDeque<io::Result<Vec<u8>>>);

impl Read for Script {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.pop_front() {
            Some(Ok(data)) => {
                buf[..data.len()].copy_from_slice(&data);
                Ok(data.len())
            }
            Some(Err(error)) => Err(error),
            None => Err(io::ErrorKind::WouldBlock.into()),
        }
    }
}

fn script(items: Vec<io::Result<Vec<u8>>>) -> Pipe<Script> {
    Pipe::new(Script(items.into()))
}

#[test]
fn a_refused_messenger_call_is_recognised_and_other_errors_are_not() {
    let refused = "main_form.rhai:8:28: Runtime error: msg::connect: no service for \
                   os.lazy.confd.v1 (tried os.lazy.confd, os.lazy.confd.v1): permission \
                   denied (EACCES) @ 'sys'";
    assert!(is_denied_call(refused));
    assert!(is_denied_call(
        "sys::confd::get: permission denied (EACCES)"
    ));
    // A script file the sandbox refuses is not a label denial.
    assert!(!is_denied_call(
        "file_read_text: permission denied (EACCES)"
    ));
    assert!(!is_denied_call(
        "msg::connect: no service for os.lazy.x (ENOENT)"
    ));
    assert!(DENIED_HINT.contains("Make LazyOS App"));
}

#[test]
fn a_pipe_returns_complete_lines_and_keeps_the_partial_one() {
    let mut pipe = script(vec![Ok(b"one\ntw".to_vec()), Ok(b"o\nthr".to_vec())]);
    assert_eq!(pipe.drain(), ["one", "two"]);
    assert!(pipe.open, "WouldBlock leaves the pipe open");
    assert!(pipe.drain().is_empty());
}

#[test]
fn end_of_stream_flushes_the_last_partial_line_and_closes() {
    let mut pipe = script(vec![Ok(b"a\nb".to_vec()), Ok(Vec::new())]);
    assert_eq!(pipe.drain(), ["a", "b"]);
    assert!(!pipe.open);
    assert!(pipe.drain().is_empty(), "a closed pipe is never read again");
}

#[test]
fn interruptions_are_retried_and_hard_errors_close_the_pipe() {
    let mut pipe = script(vec![
        Err(io::ErrorKind::Interrupted.into()),
        Ok(b"x\n".to_vec()),
        Err(io::ErrorKind::BrokenPipe.into()),
    ]);
    assert_eq!(pipe.drain(), ["x"]);
    assert!(!pipe.open);
}

#[test]
fn one_poll_reads_a_bounded_amount() {
    let chunks = (0..100).map(|_| Ok(vec![b'a'; 4096])).collect();
    let mut pipe = script(chunks);
    // Exactly the budget of 64 KiB is one overlong line, cut at MAX_LINE.
    let lines = pipe.drain();
    assert_eq!(lines.len(), 1);
    assert!(pipe.open);
    // 100 chunks were offered; the budget stopped the loop at 16.
    assert_eq!(pipe.reader.0.len(), 100 - READ_BUDGET / 4096);
}

/// Writes an executable shell script standing in for `lrplay`, which is
/// started as `player --client <project dir>`.
#[cfg(unix)]
fn fake_player(dir: &Path, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("player.sh");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh
{body}
"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn a_real_child_is_polled_to_its_exit_with_all_its_output() {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let dir = std::env::temp_dir().join(format!("lazyrad-os-launch-{}", std::process::id()));
    let player = fake_player(
        &dir,
        "echo \"args=$*\"
echo '{\"kind\":\"runtime\",\"message\":\"boom\"}' >&2
printf tail
exit 3",
    );

    let events: Arc<Mutex<Vec<RunEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let sink: EventSink = Arc::new(move |run, event| {
        assert_eq!(run, 7);
        captured.lock().unwrap().push(event);
    });
    let mut child = PollingLauncher
        .launch(&player, Path::new("/proj"), 7, sink)
        .expect("the player starts");
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.is_running() && Instant::now() < deadline {
        child.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    child.poll();

    let events = events.lock().unwrap().clone();
    assert!(
        events.contains(&RunEvent::Output("args=--client /proj".to_owned())),
        "{events:?}"
    );
    assert!(
        events.contains(&RunEvent::Output("tail".to_owned())),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RunEvent::Diagnostic(r) if r.message == "boom")),
        "{events:?}"
    );
    assert_eq!(
        events.last(),
        Some(&RunEvent::Exited(Some(3))),
        "{events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, RunEvent::Exited(_)))
            .count(),
        1,
        "exit is reported exactly once"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_missing_player_is_a_launch_error_and_kill_is_idempotent() {
    let sink: EventSink = std::sync::Arc::new(|_, _| {});
    let missing = PollingLauncher.launch(Path::new("/nonexistent/player"), Path::new("x"), 1, sink);
    assert!(missing.is_err());

    let dir = std::env::temp_dir().join(format!("lazyrad-os-kill-{}", std::process::id()));
    let player = fake_player(&dir, "sleep 30");
    let sink: EventSink = std::sync::Arc::new(|_, _| {});
    let mut child = PollingLauncher
        .launch(&player, Path::new("x"), 2, sink)
        .expect("the player starts");
    assert!(child.is_running());
    child.kill();
    child.kill();
    assert!(!child.is_running());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_player_command_line_has_client_then_debug_then_the_project() {
    let plain = player_args(Path::new("/p"), LaunchMode::Run);
    assert_eq!(plain, ["--client", "/p"]);
    let debug = player_args(Path::new("/p"), LaunchMode::Debug);
    assert_eq!(debug, ["--client", "--debug", "/p"]);
}

/// Polls `child` until it ended (or ten seconds passed).
#[cfg(unix)]
fn run_to_exit(child: &mut Box<dyn ChildProcess>) {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.is_running() && Instant::now() < deadline {
        child.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    child.poll();
}

#[cfg(unix)]
#[test]
fn a_debug_child_gets_commands_on_stdin_and_its_events_are_decoded() {
    use lazyrad_runtime::proto::Event;
    use std::sync::{Arc, Mutex};

    let dir = std::env::temp_dir().join(format!("lazyrad-os-dbg-{}", std::process::id()));
    // A player that waits for one command and answers with a protocol
    // event naming it, then prints a plain line and exits.
    let player = fake_player(
        &dir,
        r#"echo "args=$*"
read cmd
printf '%s\n' "{\"v\":1,\"event\":\"error\",\"message\":\"got $cmd\"}"
echo plain output
exit 0"#,
    );
    let events: Arc<Mutex<Vec<RunEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let sink: EventSink = Arc::new(move |_run, event| captured.lock().unwrap().push(event));
    let mut child = PollingLauncher
        .launch_with(&player, Path::new("/proj"), 3, sink, LaunchMode::Debug)
        .expect("the player starts");
    // Sent before the child has read anything: queued, not lost.
    child.send_line("hello").expect("the line is queued");
    run_to_exit(&mut child);

    let events = events.lock().unwrap().clone();
    assert!(
        events.contains(&RunEvent::Output("args=--client --debug /proj".to_owned())),
        "{events:?}"
    );
    assert!(
        events.contains(&RunEvent::Debug(Event::Error {
            message: "got hello".to_owned()
        })),
        "{events:?}"
    );
    assert!(
        events.contains(&RunEvent::Output("plain output".to_owned())),
        "program output stays output: {events:?}"
    );
    // The child is gone: sending says so rather than queueing forever.
    assert_eq!(child.send_line("late"), Err(SendError::Closed));
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_plain_run_takes_no_input_and_a_child_that_closed_stdin_reports_it() {
    use std::time::{Duration, Instant};

    let dir = std::env::temp_dir().join(format!("lazyrad-os-in-{}", std::process::id()));
    let player = fake_player(&dir, "sleep 30");
    let sink: EventSink = std::sync::Arc::new(|_, _| {});
    let mut plain = PollingLauncher
        .launch(&player, Path::new("x"), 4, sink)
        .expect("the player starts");
    assert_eq!(plain.send_line("x"), Err(SendError::Unsupported));
    plain.kill();

    // A debug child that closes its stdin without exiting: the write end
    // breaks (EPIPE) and every later line is refused, not retried.
    let closer = fake_player(&dir, "exec 0<&-\nsleep 30");
    let sink: EventSink = std::sync::Arc::new(|_, _| {});
    let mut child = PollingLauncher
        .launch_with(&closer, Path::new("x"), 5, sink, LaunchMode::Debug)
        .expect("the player starts");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut refused = None;
    while Instant::now() < deadline && refused.is_none() {
        if let Err(error) = child.send_line("{\"v\":1,\"cmd\":\"continue\"}") {
            refused = Some(error);
        }
        child.poll();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(refused, Some(SendError::Closed));
    assert_eq!(child.send_line("again"), Err(SendError::Closed));
    child.kill();
    let _ = std::fs::remove_dir_all(&dir);
}
