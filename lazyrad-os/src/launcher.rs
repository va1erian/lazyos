//! Starting the player from the IDE on LazyOS: pipes polled on the UI thread.
//!
//! The portable IDE launcher (`lazyrad_ide::run::PlayerLauncher`) reads the
//! child's pipes and waits for its exit on background threads. LazyOS does not
//! share a descriptor table between threads (the Terminal's comment in
//! `xui-app/src/bin/term.rs` says the same), so those threads lose the pipe
//! descriptors and the wait status: verified in the P3 spike, where the IDE saw
//! the player "exit" the moment it started and killed it. [`PollingLauncher`]
//! keeps everything on the thread that spawned the child: the pipes are
//! non-blocking and the IDE's window timer calls [`ChildProcess::poll`] every
//! 100 ms, which drains them, reaps an exited child and reports through the
//! same [`EventSink`].
//!
//! Spawning itself (`fork` + `execve` from a `xuid` client, which then opens its
//! own window) works: that half of the plan's spike is confirmed.

use std::io::{ErrorKind, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use lazyrad_ide::run::{
    stderr_event, ChildProcess, EventSink, LaunchError, Launcher, LineBuffer, RunEvent, RunId,
};

/// Most bytes read from one pipe per poll, so a chatty program cannot starve
/// the UI thread between frames.
const READ_BUDGET: usize = 64 * 1024;

/// Launches the player with non-blocking pipes (see the module documentation).
#[derive(Clone, Copy, Debug, Default)]
pub struct PollingLauncher;

impl Launcher for PollingLauncher {
    fn launch(
        &self,
        player: &Path,
        project_dir: &Path,
        run: RunId,
        sink: EventSink,
    ) -> Result<Box<dyn ChildProcess>, LaunchError> {
        // `--client`: inside a desktop session the compositor owns the display, so
        // the player must not race it for the grant (`init` passes the same flag
        // to every desktop app). Without it the player first tries to bind the
        // display as its owner, and that path panicked in the P3 spike.
        let mut child = Command::new(player)
            .arg("--client")
            .arg(project_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| LaunchError::new(player, source))?;
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(LaunchError::new(
                player,
                std::io::Error::other("the player's output pipes are unavailable"),
            ));
        };
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            set_nonblocking(stdout.as_raw_fd());
            set_nonblocking(stderr.as_raw_fd());
        }
        Ok(Box::new(PollingChild {
            child,
            run,
            sink,
            out: Pipe::new(stdout),
            err: Pipe::new(stderr),
            done: false,
        }))
    }
}

/// Puts `fd` into non-blocking mode so a read never parks the UI thread.
#[cfg(unix)]
fn set_nonblocking(fd: std::os::fd::RawFd) {
    // SAFETY: `fcntl(F_SETFL)` on a descriptor this process owns (it came from
    // the child's pipe, which outlives this call); it only changes the file
    // status flags and touches no memory.
    unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) };
}

/// One non-blocking pipe and the partial line read from it so far.
struct Pipe<R: Read> {
    reader: R,
    lines: LineBuffer,
    open: bool,
}

impl<R: Read> Pipe<R> {
    fn new(reader: R) -> Pipe<R> {
        Pipe {
            reader,
            lines: LineBuffer::new(),
            open: true,
        }
    }

    /// Reads what is available (up to [`READ_BUDGET`]) and returns the lines it
    /// completed. End of stream or a hard error closes the pipe; `WouldBlock`
    /// and `Interrupted` just mean "nothing more right now".
    fn drain(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut budget = READ_BUDGET;
        while self.open && budget > 0 {
            match self.reader.read(&mut chunk) {
                Ok(0) => {
                    self.open = false;
                    lines.extend(self.lines.finish());
                }
                Ok(read) => {
                    budget = budget.saturating_sub(read);
                    lines.extend(self.lines.push(&chunk[..read]));
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => {
                    self.open = false;
                    lines.extend(self.lines.finish());
                }
            }
        }
        lines
    }
}

/// The running player, driven by [`ChildProcess::poll`].
struct PollingChild {
    child: Child,
    run: RunId,
    sink: EventSink,
    out: Pipe<std::process::ChildStdout>,
    err: Pipe<std::process::ChildStderr>,
    done: bool,
}

impl PollingChild {
    /// Reports what both pipes have for us, stdout first.
    fn drain_pipes(&mut self) {
        for line in self.out.drain() {
            (self.sink)(self.run, RunEvent::Output(line));
        }
        for line in self.err.drain() {
            (self.sink)(self.run, stderr_event(line));
        }
    }
}

impl ChildProcess for PollingChild {
    fn kill(&mut self) {
        if !self.done {
            let _ = self.child.kill();
            // Reap now so End leaves no zombie; events of a killed run are stale
            // and dropped by the IDE, so nothing is reported.
            let _ = self.child.wait();
            self.done = true;
        }
    }

    fn is_running(&mut self) -> bool {
        !self.done && matches!(self.child.try_wait(), Ok(None))
    }

    fn poll(&mut self) {
        if self.done {
            return;
        }
        self.drain_pipes();
        match self.child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => {
                // The child is gone; anything it wrote last is still in the pipe.
                self.drain_pipes();
                self.done = true;
                (self.sink)(self.run, RunEvent::Exited(status.code()));
            }
            Err(_) => {
                self.done = true;
                (self.sink)(self.run, RunEvent::Exited(None));
            }
        }
    }
}

impl Drop for PollingChild {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
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
        let missing =
            PollingLauncher.launch(Path::new("/nonexistent/player"), Path::new("x"), 1, sink);
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
}
