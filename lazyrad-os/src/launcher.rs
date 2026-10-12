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
//!
//! # Debugging
//!
//! [`LaunchMode::Debug`] starts the player with `--debug` and a piped stdin.
//! Commands go out through [`ChildProcess::send_line`] into an
//! [`InputQueue`] over the pipe's non-blocking write end, flushed again on every
//! poll, so a full pipe or a slow player costs the window nothing; the player's
//! protocol events come back on stdout and are decoded by `stdout_event`.

use std::io::{ErrorKind, Read};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};

use lazyrad_ide::run::{
    stderr_event, stdout_event, ChildProcess, EventSink, LaunchError, LaunchMode, Launcher,
    LineBuffer, RunEvent, RunId, SendError,
};

use crate::input::InputQueue;

/// Most bytes read from one pipe per poll, so a chatty program cannot starve
/// the UI thread between frames.
const READ_BUDGET: usize = 64 * 1024;

/// What the run console adds when a Messenger call of the project is refused.
///
/// Play forks the player, and a child inherits its parent's label, so the
/// project runs with the IDE's permissions (`os.lazy.lazyrad`), not the ones
/// the installed app would get: a service the IDE's manifest does not name is
/// refused (`docs/lazyrad-package-plan.md`, section 1). The installed app is
/// granted what its scripts call, so the same project works once installed.
pub const DENIED_HINT: &str = "Note: Play runs your project with LazyRAD's own permissions, so a \
    service LazyRAD does not declare is refused. An app made with Make LazyOS App is granted the \
    services its scripts call, and runs without this refusal.";

/// Whether `line` is the player reporting a refused Messenger call: the
/// `msg::`/`sys::` binding naming the kernel's permission denial.
fn is_denied_call(line: &str) -> bool {
    line.contains("permission denied (EACCES)")
        && (line.contains("msg::") || line.contains("sys::"))
}

/// Launches the player with non-blocking pipes (see the module documentation).
#[derive(Clone, Copy, Debug, Default)]
pub struct PollingLauncher;

/// The player's command line: `--client` always (inside a desktop session the
/// compositor owns the display, so the player must not race it for the grant;
/// `init` passes the same flag to every desktop app, and without it the player
/// first tries to bind the display as its owner, a path that panicked in the P3
/// spike), then `--debug` for a debug run, then the project.
pub fn player_args(project_dir: &Path, mode: LaunchMode) -> Vec<std::ffi::OsString> {
    let mut args = vec![std::ffi::OsString::from("--client")];
    if mode == LaunchMode::Debug {
        args.push("--debug".into());
    }
    args.push(project_dir.as_os_str().to_owned());
    args
}

impl Launcher for PollingLauncher {
    fn launch(
        &self,
        player: &Path,
        project_dir: &Path,
        run: RunId,
        sink: EventSink,
    ) -> Result<Box<dyn ChildProcess>, LaunchError> {
        self.launch_with(player, project_dir, run, sink, LaunchMode::Run)
    }

    fn launch_with(
        &self,
        player: &Path,
        project_dir: &Path,
        run: RunId,
        sink: EventSink,
        mode: LaunchMode,
    ) -> Result<Box<dyn ChildProcess>, LaunchError> {
        let debugging = mode == LaunchMode::Debug;
        let mut child = Command::new(player)
            .args(player_args(project_dir, mode))
            .stdin(if debugging {
                Stdio::piped()
            } else {
                Stdio::null()
            })
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
        let stdin = child.stdin.take();
        if debugging && stdin.is_none() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(LaunchError::new(
                player,
                std::io::Error::other("the player's input pipe is unavailable"),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            set_nonblocking(stdout.as_raw_fd());
            set_nonblocking(stderr.as_raw_fd());
            if let Some(stdin) = &stdin {
                // The write end must not block the UI thread either.
                set_nonblocking(stdin.as_raw_fd());
            }
        }
        let input = stdin.map(|stdin| {
            let mut queue = InputQueue::new();
            queue.attach(stdin);
            queue
        });
        Ok(Box::new(PollingChild {
            child,
            run,
            sink,
            out: Pipe::new(stdout),
            err: Pipe::new(stderr),
            input,
            debugging,
            done: false,
            hinted: false,
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

/// One non-blocking pipe and the partial line read from it so far (also the
/// development run's, [`crate::devplay`]).
pub(crate) struct Pipe<R: Read> {
    reader: R,
    lines: LineBuffer,
    open: bool,
}

impl<R: Read> Pipe<R> {
    pub(crate) fn new(reader: R) -> Pipe<R> {
        Pipe {
            reader,
            lines: LineBuffer::new(),
            open: true,
        }
    }

    /// Reads what is available (up to [`READ_BUDGET`]) and returns the lines it
    /// completed. End of stream or a hard error closes the pipe; `WouldBlock`
    /// and `Interrupted` just mean "nothing more right now".
    pub(crate) fn drain(&mut self) -> Vec<String> {
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
    /// The debug run's stdin; `None` for a plain run.
    input: Option<InputQueue<ChildStdin>>,
    /// Whether stdout carries debug-protocol events.
    debugging: bool,
    done: bool,
    /// [`DENIED_HINT`] was already shown for this run.
    hinted: bool,
}

impl PollingChild {
    /// Reports what both pipes have for us, stdout first.
    fn drain_pipes(&mut self) {
        for line in self.out.drain() {
            let event = stdout_event(line.clone(), self.debugging);
            self.report(event, &line);
        }
        for line in self.err.drain() {
            self.report(stderr_event(line.clone()), &line);
        }
    }

    /// Sends `event` (made from `line`) and, the first time a line is a
    /// refused Messenger call, the explanation after it.
    fn report(&mut self, event: RunEvent, line: &str) {
        (self.sink)(self.run, event);
        if !self.hinted && is_denied_call(line) {
            self.hinted = true;
            (self.sink)(self.run, RunEvent::Output(DENIED_HINT.to_owned()));
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

    fn send_line(&mut self, line: &str) -> Result<(), SendError> {
        if self.done {
            return Err(SendError::Closed);
        }
        match self.input.as_mut() {
            Some(input) => input.push_line(line),
            None => Err(SendError::Unsupported),
        }
    }

    fn poll(&mut self) {
        if self.done {
            return;
        }
        // Whatever the pipe could not take last time.
        if let Some(input) = self.input.as_mut() {
            input.flush();
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
mod tests;
