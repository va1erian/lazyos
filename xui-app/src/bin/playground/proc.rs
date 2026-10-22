//! The process side of the run table: `rhai` children on pipes
//! (`docs/messenger-playground-plan.md` D4).
//!
//! Each run is `rhai -` with the composed script (prelude, then the user's
//! text) written to its standard input, which is closed after the last byte so
//! `rhai` starts. Nothing is written to disk: there is no temp file to leave
//! behind if the window dies. Three helper threads per run copy the pipes: one
//! writes the script, two read standard output and standard error line by
//! line into a channel the UI thread drains ([`Runner::poll`]) and ring the
//! window through its [`Proxy`]. A fourth reaps the child once both pipes are
//! closed.
//!
//! Stop is `SIGKILL` (Rhai may be in a blocking `msg::call`). The child is
//! kept in a mutex and reaped only under it, so a kill never reaches a
//! recycled task slot: the child is unreaped, hence still ours, whenever the
//! lock holds `Some`. Dropping the runner kills every live child, which is how
//! closing the window leaves no `rhai` behind.
//!
//! When the window cannot run that code (it is killed, or it panics), each
//! child has asked the kernel to `SIGKILL` it when its parent process exits
//! (`PR_SET_PDEATHSIG`, set between `fork` and `exec` and kept by `execve`):
//! a `loop {}` script does not outlive the Playground. This is best effort:
//! a kernel that refuses the call (`EINVAL`) leaves the child as it was.
//!
//! Output is bounded at every step so a script that prints without end cannot
//! take the window's memory or time. Each run has its own bounded channel
//! ([`QUEUE`] events of at most [`MAX_LINE`] characters): a full channel blocks
//! that run's reader threads, the pipe fills, and the child blocks in `write`.
//! The UI thread is woken at most once until it has looked ([`Wake`]) and takes
//! at most [`BATCH`] events per look, round-robin over the runs, asking for
//! another look when it stopped early.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use xui_core::app::Proxy;
use xui_playground::runner::{RunId, Runner, RunnerEvent, SpawnRequest, Stream, MAX_LINE};

use crate::msg::Msg;

/// How often the reaper looks at a child whose pipes are closed.
const REAP_POLL: Duration = Duration::from_millis(25);
/// Longest line read before it is cut (the table keeps `MAX_LINE`).
const READ_LINE_MAX: usize = 64 * 1024;

/// Events a run may have queued for the UI thread before its readers block.
const QUEUE: usize = 256;
/// Events the UI thread takes in one look.
const BATCH: usize = 512;

type Shared = Arc<Mutex<Option<Child>>>;

/// At most one [`Msg::RunnerWake`] is in flight: a producer sends it only when
/// none is pending, and the UI thread clears the flag before it drains, so an
/// event queued after the drain started always brings a new wake.
#[derive(Clone)]
struct Wake {
    pending: Arc<AtomicBool>,
    proxy: Proxy<Msg>,
}

impl Wake {
    /// Ask for a look, unless one is already asked for.
    fn ring(&self) {
        if !self.pending.swap(true, Ordering::SeqCst) {
            let _ = self.proxy.send(Msg::RunnerWake);
        }
    }

    /// The UI thread is about to drain.
    fn looking(&self) {
        self.pending.store(false, Ordering::SeqCst);
    }
}

/// One live run: its child and the events it queued.
struct Live {
    child: Shared,
    events: Receiver<RunnerEvent>,
}

/// Starts `rhai` children and hands their output to the UI thread.
pub struct ProcRunner {
    program: String,
    wake: Wake,
    children: HashMap<RunId, Live>,
}

impl ProcRunner {
    pub fn new(proxy: Proxy<Msg>) -> ProcRunner {
        ProcRunner {
            program: fhs::bin::RHAI.to_string(),
            wake: Wake {
                pending: Arc::new(AtomicBool::new(false)),
                proxy,
            },
            children: HashMap::new(),
        }
    }

    fn kill_all(&mut self) {
        for live in self.children.values() {
            kill(&live.child);
        }
    }
}

impl Drop for ProcRunner {
    fn drop(&mut self) {
        self.kill_all();
    }
}

/// SIGKILL the child if it is still unreaped.
fn kill(child: &Shared) {
    if let Ok(mut guard) = child.lock() {
        if let Some(child) = guard.as_mut() {
            let _ = child.kill();
        }
    }
}

/// Read `source` line by line (lossy UTF-8), sending each to the table.
fn pump(source: impl Read, id: RunId, stream: Stream, sender: SyncSender<RunnerEvent>, wake: Wake) {
    let mut reader = BufReader::new(source);
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        let mut limited = (&mut reader).take(READ_LINE_MAX as u64);
        match limited.read_until(b'\n', &mut bytes) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        while matches!(bytes.last(), Some(b'\n' | b'\r')) {
            bytes.pop();
        }
        // The table keeps MAX_LINE characters; do not queue more than that.
        let line: String = String::from_utf8_lossy(&bytes)
            .chars()
            .take(MAX_LINE)
            .collect();
        // Blocks while the run's queue is full; fails once the runner is gone.
        if sender
            .send(RunnerEvent::Output { id, stream, line })
            .is_err()
        {
            break;
        }
        wake.ring();
    }
}

/// The command for the run's child: `rhai -`, its pipes, and (on unix) the
/// parent-death signal.
fn command(program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        // SAFETY: the closure runs in the forked child before `exec` and makes
        // one async-signal-safe system call. A refusal is ignored: the child
        // then lives as it did before this call existed (the window still
        // kills it on Stop and on exit).
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong);
                Ok(())
            });
        }
    }
    command
}

impl Runner for ProcRunner {
    fn spawn(&mut self, request: &SpawnRequest) -> Result<(), String> {
        let mut child = command(&self.program)
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", self.program))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let shared: Shared = Arc::new(Mutex::new(Some(child)));
        let (sender, events) = sync_channel(QUEUE);
        self.children.insert(
            request.id,
            Live {
                child: Arc::clone(&shared),
                events,
            },
        );
        let id = request.id;

        let source = request.source.clone();
        if let Some(mut stdin) = stdin {
            std::thread::spawn(move || {
                // Dropping the handle closes the pipe: rhai starts at EOF.
                let _ = stdin.write_all(source.as_bytes());
            });
        }
        let mut readers = Vec::new();
        if let Some(out) = stdout {
            let (s, w) = (sender.clone(), self.wake.clone());
            readers.push(std::thread::spawn(move || {
                pump(out, id, Stream::Stdout, s, w)
            }));
        }
        if let Some(err) = stderr {
            let (s, w) = (sender.clone(), self.wake.clone());
            readers.push(std::thread::spawn(move || {
                pump(err, id, Stream::Stderr, s, w)
            }));
        }
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            for reader in readers {
                let _ = reader.join();
            }
            let code = loop {
                let mut guard = match shared.lock() {
                    Ok(guard) => guard,
                    Err(_) => break None,
                };
                let Some(child) = guard.as_mut() else {
                    break None;
                };
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let code = status.code();
                        *guard = None;
                        break code;
                    }
                    Ok(None) => {}
                    Err(_) => {
                        *guard = None;
                        break None;
                    }
                }
                drop(guard);
                std::thread::sleep(REAP_POLL);
            };
            if sender.send(RunnerEvent::Exit { id, code }).is_ok() {
                wake.ring();
            }
        });
        Ok(())
    }

    /// Take at most [`BATCH`] events, one run after another in turn so a
    /// chatty run does not starve the rest; when events may remain, ask for
    /// another look.
    fn poll(&mut self, out: &mut Vec<RunnerEvent>) {
        self.wake.looking();
        let mut ids: Vec<RunId> = self.children.keys().copied().collect();
        ids.sort();
        let mut budget = BATCH;
        let mut more = false;
        'turns: loop {
            let mut took = false;
            for id in &ids {
                let Some(live) = self.children.get(id) else {
                    continue;
                };
                if budget == 0 {
                    more = true;
                    break 'turns;
                }
                match live.events.try_recv() {
                    Ok(event) => {
                        took = true;
                        budget -= 1;
                        // Exit is the last event of a run: nothing follows it.
                        if matches!(event, RunnerEvent::Exit { .. }) {
                            self.children.remove(id);
                        }
                        out.push(event);
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        self.children.remove(id);
                    }
                }
            }
            if !took {
                break;
            }
        }
        if more {
            self.wake.ring();
        }
    }

    fn stop(&mut self, id: RunId) {
        if let Some(live) = self.children.get(&id) {
            kill(&live.child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process-less `Wake` cannot be built (it needs a window's proxy), so
    /// the drain's rules are tested on the pieces: the bound is a bound.
    #[test]
    fn the_queue_and_the_batch_are_bounded() {
        assert!(QUEUE >= 1 && BATCH >= QUEUE);
        let (sender, events) = sync_channel::<RunnerEvent>(QUEUE);
        for i in 0..QUEUE {
            sender
                .try_send(RunnerEvent::Output {
                    id: RunId(1),
                    stream: Stream::Stdout,
                    line: i.to_string(),
                })
                .unwrap();
        }
        let full = sender.try_send(RunnerEvent::Exit {
            id: RunId(1),
            code: Some(0),
        });
        assert!(
            full.is_err(),
            "a run's queue holds {QUEUE} events and no more"
        );
        drop(events);
    }
}
