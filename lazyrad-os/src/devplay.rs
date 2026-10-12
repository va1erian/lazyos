//! Play under the project's own permissions (issue #529,
//! `docs/lazyrad-package-plan.md` section 3).
//!
//! Once the IDE is a package (`app:os.lazy.lazyrad`), a player it forks would
//! inherit the IDE's label: the project's `sys::*` calls would be judged
//! against the IDE's permissions and its `msg::serve` names would land in the
//! IDE's namespace. [`DevLauncher`] instead runs the player under the label
//! the installed app would get, `dev:<system_name>`, and keeps its pipes:
//!
//! 1. derive the package exactly as File → Make LazyOS App does
//!    (`lazyrad_ide::make_app::build`: the project, the player, the
//!    interfaces and topics the scripts use) and write it to `/transient`;
//! 2. ask for approval: `mimed.Open(path, "develop")` starts the Installer's
//!    development consent, which calls `pkgd.Develop`. A rule set this session
//!    already approved (or a narrower one) is loaded without a window;
//! 3. wait for `pkgd`'s `system/events/pkg/develop` record naming the label
//!    (or a `denied` one for the package);
//! 4. `spawnv` the player with `AS_LABELLED "dev:<system_name>"`, its stdout and
//!    stderr on pipes this IDE reads ([`xui_app::sys::spawn_labelled`]). The
//!    kernel lets the IDE do it only because its manifest says
//!    `develop = true` and `pkgd` holds an approved rule set for the label.
//!
//! The run console shows each step. [`crate::playdev::launcher_for_this_process`]
//! is the seam: an unlabelled IDE (an image without the LazyRAD package, phase
//! A of the plan) keeps the plain fork of `PollingLauncher`. Unix only (the
//! player's pipes and `waitpid`); a host build of the IDE never runs labelled.

use std::fs::File;
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use lazyrad_ide::run::{
    stderr_event, stdout_event, ChildProcess, EventSink, LaunchError, LaunchMode, Launcher,
    RunEvent, RunId, SendError,
};
use rhai::{Dynamic, Map};
use rhai_lazy::msg::gate::Gate;
use rhai_lazy::msg::{topics, Fabric, Service, Subscription, Wait};
use xui_app::sys;

use crate::input::InputQueue;
use crate::launcher::{player_args, Pipe};

/// Where the development package is written: readable by `pkgd` by design.
pub const STAGING_DIR: &str = fhs::mount::TRANSIENT;
/// The events `pkgd` publishes (`idl/pkgd.midl`).
const PKG_EVENTS: &str = "system/events/pkg/+";
/// How long a run waits for the user to answer the consent, in PIT ticks.
const APPROVAL_TICKS: u64 = 300 * 100;
/// The environment the player keeps.
const KEPT_ENV: &[&str] = &["HOME", "USER", "LOGNAME", "PATH", "LANG", "TERM"];

/// See the module documentation.
pub struct DevLauncher {
    author: String,
}

impl DevLauncher {
    pub fn new(author: &str) -> DevLauncher {
        DevLauncher {
            author: author.to_owned(),
        }
    }
}

/// A launch failure as the IDE reports it.
fn launch_error(player: &Path, text: impl Into<String>) -> LaunchError {
    LaunchError::new(player, std::io::Error::other(text.into()))
}

/// The `.lrp` file of `project_dir` (the first one, by name).
fn project_file(project_dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(project_dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "lrp"))
        .collect();
    found.sort();
    found.into_iter().next()
}

impl Launcher for DevLauncher {
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
        let lrp = project_file(project_dir)
            .ok_or_else(|| launch_error(player, "the project folder has no .lrp file"))?;
        let built = lazyrad_ide::make_app::build(&lrp, Some(player), &self.author)
            .map_err(|why| launch_error(player, why))?;
        let staged = Path::new(STAGING_DIR).join(format!("lazyrad-dev-{}.lzp", built.system_name));
        std::fs::write(&staged, &built.bytes)
            .map_err(|error| launch_error(player, format!("{}: {error}", staged.display())))?;
        let fabric = Gate::detect()
            .map(|gate| Rc::new(Fabric::new(Rc::new(gate))))
            .ok_or_else(|| launch_error(player, "not running on LazyOS"))?;
        // Subscribe before asking, so the answer cannot be missed.
        let events = topics::subscribe(&fabric, PKG_EVENTS, &Map::new())
            .map_err(|error| launch_error(player, format!("the package events: {error}")))?;
        let path = staged.to_string_lossy().into_owned();
        ask_for_approval(&fabric, &path).map_err(|why| launch_error(player, why))?;
        let label = format!("dev:{}", built.system_name);
        (sink)(
            run,
            RunEvent::Output(format!(
                "Waiting for approval to run {} under {label}…",
                built.system_name
            )),
        );
        Ok(Box::new(DevChild {
            run,
            sink,
            // Commands sent while the approval is pending wait here and go to
            // the player's stdin once it exists.
            input: InputQueue::new(),
            debugging: mode == LaunchMode::Debug,
            state: State::Waiting(Waiting {
                events,
                label,
                system_name: built.system_name,
                deadline: sys::clock_ticks().saturating_add(APPROVAL_TICKS),
                player: player.to_path_buf(),
                project_dir: project_dir.to_path_buf(),
                mode,
                _fabric: fabric,
            }),
        }))
    }
}

/// `mimed.Open(path, "develop")`: the Installer's development consent.
fn ask_for_approval(fabric: &Rc<Fabric>, path: &str) -> Result<(), String> {
    let mimed = Service::connect(Rc::clone(fabric), "os.lazy.mimed.v1", None)
        .map_err(|error| format!("the file-type service: {error}"))?;
    let args = Dynamic::from_array(vec![
        Dynamic::from(path.to_owned()),
        Dynamic::from("develop".to_owned()),
    ]);
    let reply = mimed
        .invoke("Open", args)
        .map_err(|error| format!("asking for approval: {error}"))?;
    let launched = reply
        .try_cast::<Map>()
        .and_then(|map| map.get("launched").and_then(|v| v.as_bool().ok()));
    if launched == Some(true) {
        Ok(())
    } else {
        Err("the Installer could not be started to approve the run".to_owned())
    }
}

/// A run waiting for its approval.
struct Waiting {
    events: Subscription,
    label: String,
    system_name: String,
    deadline: u64,
    player: PathBuf,
    project_dir: PathBuf,
    /// Plain or debug run: decides the player's arguments and its stdin.
    mode: LaunchMode,
    /// Keeps the fabric (and so the subscription's broker) alive.
    _fabric: Rc<Fabric>,
}

/// What `pkgd` said about this run.
enum Answer {
    Approved,
    Refused(String),
}

impl Waiting {
    /// The answer among the queued package events, if it came.
    fn answer(&self) -> Option<Answer> {
        loop {
            let event = self.events.next(Wait::Ms(0)).ok()?;
            let map = event.try_cast::<Map>()?;
            let text =
                |map: &Map, key: &str| map.get(key).map(|v| v.to_string()).unwrap_or_default();
            let topic = text(&map, "topic");
            let Some(payload) = map.get("payload").and_then(|p| p.clone().try_cast::<Map>()) else {
                continue;
            };
            let ok = payload.get("ok").and_then(|v| v.as_bool().ok()) == Some(true);
            if topic.ends_with("/develop") && ok && text(&payload, "detail") == self.label {
                return Some(Answer::Approved);
            }
            if topic.ends_with("/denied") && text(&payload, "system_name") == self.system_name {
                return Some(Answer::Refused(text(&payload, "detail")));
            }
        }
    }
}

/// The player, running under its development label.
struct Running {
    pid: i32,
    out: Pipe<File>,
    err: Pipe<File>,
}

enum State {
    Waiting(Waiting),
    Running(Running),
    Done,
}

/// A development run: first waiting for approval, then the player.
struct DevChild {
    run: RunId,
    sink: EventSink,
    /// The player's stdin (a debug run); lines are held until it is attached.
    input: InputQueue<File>,
    /// Whether the player's stdout carries debug-protocol events.
    debugging: bool,
    state: State,
}

impl DevChild {
    fn say(&self, line: String) {
        (self.sink)(self.run, RunEvent::Output(line));
    }

    fn finish(&mut self, code: Option<i32>) {
        self.state = State::Done;
        (self.sink)(self.run, RunEvent::Exited(code));
    }

    /// One look at the approval; starts the player once it came.
    fn poll_waiting(&mut self) {
        let State::Waiting(waiting) = &self.state else {
            return;
        };
        let outcome = match waiting.answer() {
            Some(Answer::Approved) => match start(waiting) {
                Ok((running, stdin)) => {
                    if let Some(stdin) = stdin {
                        // What the IDE queued during the approval is written now.
                        self.input.attach(stdin);
                    }
                    Ok((running, format!("Running under {}.", waiting.label)))
                }
                Err(why) => Err(format!("The run could not start: {why}")),
            },
            Some(Answer::Refused(why)) => Err(format!("The run was refused: {why}")),
            None if sys::clock_ticks() >= waiting.deadline => {
                Err("The run was not approved in time.".to_owned())
            }
            None => return,
        };
        match outcome {
            Ok((running, line)) => {
                self.say(line);
                self.state = State::Running(running);
            }
            Err(line) => {
                self.say(line);
                self.finish(None);
            }
        }
    }

    fn poll_running(&mut self) {
        let State::Running(running) = &mut self.state else {
            return;
        };
        // Whatever the stdin pipe could not take last time.
        self.input.flush();
        let debugging = self.debugging;
        let stdout = |line: String| stdout_event(line, debugging);
        let mut lines: Vec<RunEvent> = running.out.drain().into_iter().map(stdout).collect();
        lines.extend(running.err.drain().into_iter().map(stderr_event));
        let exited = reap(running.pid, false);
        if exited.is_some() {
            // Anything written last is still in the pipes.
            lines.extend(running.out.drain().into_iter().map(stdout));
            lines.extend(running.err.drain().into_iter().map(stderr_event));
        }
        for line in lines {
            (self.sink)(self.run, line);
        }
        if let Some(code) = exited {
            self.finish(code);
        }
    }
}

impl ChildProcess for DevChild {
    fn kill(&mut self) {
        if let State::Running(running) = &self.state {
            // SAFETY: `kill(2)` on the pid of a child this task spawned and
            // has not reaped yet (so the pid cannot name another process); it
            // touches no memory.
            unsafe { libc::kill(running.pid, libc::SIGKILL) };
            let _ = reap(running.pid, true);
        }
        self.state = State::Done;
    }

    fn is_running(&mut self) -> bool {
        !matches!(self.state, State::Done)
    }

    fn send_line(&mut self, line: &str) -> Result<(), SendError> {
        if matches!(self.state, State::Done) {
            return Err(SendError::Closed);
        }
        if !self.debugging {
            return Err(SendError::Unsupported);
        }
        self.input.push_line(line)
    }

    fn poll(&mut self) {
        match self.state {
            State::Waiting(_) => self.poll_waiting(),
            State::Running(_) => self.poll_running(),
            State::Done => {}
        }
    }
}

impl Drop for DevChild {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Reap `pid`: `Some(exit code)` once it ended (`Some(None)` when killed by a
/// signal), `None` while it runs (`block` waits).
fn reap(pid: i32, block: bool) -> Option<Option<i32>> {
    let mut status = 0;
    let flags = if block { 0 } else { libc::WNOHANG };
    // SAFETY: `waitpid(2)` for this task's own child, writing the status into
    // a local that outlives the call.
    let reaped = unsafe { libc::waitpid(pid, &mut status, flags) };
    if reaped == pid {
        Some(libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status)))
    } else if reaped < 0 {
        Some(None)
    } else {
        None
    }
}

/// A pipe as `(read, write)` descriptors, both close-on-exec (the labelled
/// child gets its end through `spawnv`'s stdio words, never by exec).
/// `nonblocking_end` picks the end this task keeps and polls (the other is the
/// child's and stays blocking, so the player blocks on a full pipe rather than
/// failing its write): the read end for a pipe the player writes to, the write
/// end for one it reads.
fn pipe_with(nonblocking_end: End) -> Result<(i32, i32), String> {
    let mut fds = [0i32; 2];
    // SAFETY: `pipe2(2)` writes two descriptors into the local array.
    let code = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if code != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let ours = match nonblocking_end {
        End::Read => fds[0],
        End::Write => fds[1],
    };
    // SAFETY: `fcntl(F_SETFL)` on a descriptor `pipe2` just returned; it only
    // changes the descriptor's status flags.
    unsafe { libc::fcntl(ours, libc::F_SETFL, libc::O_NONBLOCK) };
    Ok((fds[0], fds[1]))
}

/// Which end of a pipe this task keeps (and makes non-blocking).
#[derive(Clone, Copy)]
enum End {
    Read,
    Write,
}

/// A pipe the player writes (stdout, stderr): we read it.
fn pipe() -> Result<(i32, i32), String> {
    pipe_with(End::Read)
}

/// Close a descriptor this task owns.
fn close(fd: i32) {
    // SAFETY: `close(2)` on a descriptor `pipe` returned to this task and
    // nothing else holds.
    unsafe { libc::close(fd) };
}

/// Start the player under the approved label with fresh pipes: stdout and
/// stderr always, stdin too for a debug run (returned as our write end).
fn start(waiting: &Waiting) -> Result<(Running, Option<File>), String> {
    let cred =
        sys::cred_get(None).map_err(|code| format!("reading this task's identity: {code}"))?;
    // Same uid, gid and session; no capability is passed on.
    let child_cred = sys::Cred { caps: 0, ..cred };
    let (out_read, out_write) = pipe()?;
    let (err_read, err_write) = match pipe() {
        Ok(pair) => pair,
        Err(why) => {
            close(out_read);
            close(out_write);
            return Err(why);
        }
    };
    // A debug run also gets a stdin pipe the IDE writes its commands to.
    let (in_read, in_write) = if waiting.mode == LaunchMode::Debug {
        match pipe_with(End::Write) {
            Ok((read, write)) => (Some(read), Some(write)),
            Err(why) => {
                close(out_read);
                close(out_write);
                close(err_read);
                close(err_write);
                return Err(why);
            }
        }
    } else {
        (None, None)
    };
    let player = waiting.player.to_string_lossy().into_owned();
    let args: Vec<String> = player_args(&waiting.project_dir, waiting.mode)
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let mut argv: Vec<&str> = vec![player.as_str()];
    argv.extend(args.iter().map(String::as_str));
    let env: Vec<String> = std::env::vars()
        .filter(|(key, _)| KEPT_ENV.contains(&key.as_str()))
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    let envp: Vec<&str> = env.iter().map(String::as_str).collect();
    let spawned = sys::spawn_labelled(
        &player,
        &argv,
        &envp,
        child_cred,
        &waiting.label,
        [in_read, Some(out_write), Some(err_write)],
    );
    // The child holds its own copies of its ends now (or never will).
    close(out_write);
    close(err_write);
    if let Some(in_read) = in_read {
        close(in_read);
    }
    match spawned {
        Ok(pid) => Ok((
            Running {
                pid: pid as i32,
                // SAFETY: the read ends came from `pipe` and are owned by
                // nothing else; each `File` closes its descriptor when dropped.
                out: Pipe::new(unsafe { File::from_raw_fd(out_read) }),
                // SAFETY: as above.
                err: Pipe::new(unsafe { File::from_raw_fd(err_read) }),
            },
            // SAFETY: the write end came from `pipe_with` and nothing else
            // owns it; the `File` closes it when the queue drops it, which the
            // player reads as end of input.
            in_write.map(|fd| unsafe { File::from_raw_fd(fd) }),
        )),
        Err(code) => {
            close(out_read);
            close(err_read);
            if let Some(in_write) = in_write {
                close(in_write);
            }
            Err(match -code {
                13 => format!(
                    "the system refused {} (is `develop = true` in the IDE's manifest, and was the run approved?)",
                    waiting.label
                ),
                errno => format!("spawnv failed with errno {errno}"),
            })
        }
    }
}

#[cfg(test)]
mod tests;
