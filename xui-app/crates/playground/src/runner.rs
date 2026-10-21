//! The run table: the child `rhai` processes a window started, their output
//! and exit status, error positions mapped back to editor lines, Stop, and
//! the `PLAYGROUND:RUN` marker payloads (`docs/messenger-playground-plan.md`
//! D4, D7).
//!
//! Starting a process is the window's job: it implements [`Runner`] over the
//! LazyOS spawn call and pipes. The table is the model above it, tested here
//! against [`crate::runner_fake::FakeRunner`].

use std::collections::VecDeque;

use crate::run_map::{compose_auto, Composed, ErrorMark};
use crate::util::fnv1a64;

/// Lines of output a run keeps; older lines are counted and dropped.
pub const MAX_OUTPUT_LINES: usize = 5_000;
/// Runs the table keeps once they ended; the oldest are forgotten.
pub const MAX_FINISHED: usize = 32;
/// Characters of one output line kept.
pub const MAX_LINE: usize = 4_096;
/// Runs that may be live at once. A child that never ends (a server, a
/// `loop {}`) is a process the window must kill; this keeps a held F5 or a
/// script of a hundred runs from filling the task table.
pub const MAX_LIVE: usize = 8;

/// The window's run number: 1, 2, 3, ...
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RunId(pub u32);

/// A request to start a child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnRequest {
    pub id: RunId,
    /// The text to give `rhai`: prelude, then the script.
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// What a child did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerEvent {
    Output {
        id: RunId,
        stream: Stream,
        line: String,
    },
    /// The child ended; `None` when a signal (Stop) ended it.
    Exit { id: RunId, code: Option<i32> },
}

/// The process side.
pub trait Runner {
    /// Start `rhai` on the request's source.
    fn spawn(&mut self, request: &SpawnRequest) -> Result<(), String>;
    /// Move what happened since the last call into `out`.
    fn poll(&mut self, out: &mut Vec<RunnerEvent>);
    /// Ask the child to end (SIGKILL: Rhai may be in a blocking call).
    fn stop(&mut self, id: RunId);
}

/// Where a run stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunState {
    Running,
    /// The user pressed Stop and the child has not ended yet.
    Stopping,
    Exited(i32),
    /// Ended by Stop.
    Stopped,
    /// Never started.
    Failed(String),
}

impl RunState {
    pub fn is_live(&self) -> bool {
        matches!(self, RunState::Running | RunState::Stopping)
    }

    /// The status words of the marker and the tab: `0`, `1`, `stopped`,
    /// `failed`; `None` while it runs.
    pub fn status_text(&self) -> Option<String> {
        match self {
            RunState::Running | RunState::Stopping => None,
            RunState::Exited(code) => Some(code.to_string()),
            RunState::Stopped => Some("stopped".to_string()),
            RunState::Failed(_) => Some("failed".to_string()),
        }
    }
}

/// One line of a run's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputLine {
    pub stream: Stream,
    pub text: String,
    /// Where an error line points in the editor.
    pub mark: Option<ErrorMark>,
}

/// One run.
#[derive(Debug, Clone)]
pub struct Run {
    pub id: RunId,
    /// What the tab shows (the buffer's name).
    pub title: String,
    pub state: RunState,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    pub output: VecDeque<OutputLine>,
    /// Lines dropped from the front of `output`.
    pub dropped_lines: u64,
    /// Lines the child wrote in all (the per-run counter).
    pub lines_total: u64,
    /// Hash of the script text, for the marker.
    pub hash: u64,
    composed: Composed,
}

impl Run {
    /// The error marks of this run, oldest first.
    pub fn marks(&self) -> impl Iterator<Item = &ErrorMark> {
        self.output.iter().filter_map(|l| l.mark.as_ref())
    }

    /// Seconds the run took (or has taken).
    pub fn elapsed_ms(&self, now_ms: u64) -> u64 {
        self.ended_ms
            .unwrap_or(now_ms)
            .saturating_sub(self.started_ms)
    }
}

/// Start or end of a run, for the trace line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunMarker {
    pub id: RunId,
    pub hash: u64,
    /// `None` for a start.
    pub status: Option<String>,
}

impl RunMarker {
    /// The arguments of `PLAYGROUND:RUN:`, `3:START:<hash>` or
    /// `3:EXIT:<status>:<hash>`. Never the script, its arguments or output.
    pub fn args(&self) -> String {
        match &self.status {
            None => format!("{}:START:{:016x}", self.id.0, self.hash),
            Some(status) => format!("{}:EXIT:{status}:{:016x}", self.id.0, self.hash),
        }
    }
}

/// The refusal when [`MAX_LIVE`] runs are live.
pub fn too_many_live() -> String {
    format!("{MAX_LIVE} runs are already live: Stop one first.")
}

/// The runs a window started.
pub struct RunTable<R: Runner> {
    runner: R,
    runs: Vec<Run>,
    next: u32,
    markers: Vec<RunMarker>,
}

impl<R: Runner> RunTable<R> {
    pub fn new(runner: R) -> RunTable<R> {
        RunTable {
            runner,
            runs: Vec::new(),
            next: 1,
            markers: Vec::new(),
        }
    }

    pub fn runner(&self) -> &R {
        &self.runner
    }

    pub fn runner_mut(&mut self) -> &mut R {
        &mut self.runner
    }

    /// Start `script` (the buffer text or the selection, whose first line is
    /// editor line `first_line`). The caller has already passed the review
    /// gate; the table does not run anything on its own. With [`MAX_LIVE`]
    /// runs live it refuses and records nothing (no run, no marker).
    pub fn start(
        &mut self,
        script: &str,
        title: &str,
        first_line: usize,
        now_ms: u64,
    ) -> Result<RunId, String> {
        if self.live_count() >= MAX_LIVE {
            return Err(too_many_live());
        }
        let id = RunId(self.next);
        self.next += 1;
        let composed = compose_auto(script, first_line);
        let hash = fnv1a64(script.as_bytes());
        let mut run = Run {
            id,
            title: title.to_string(),
            state: RunState::Running,
            started_ms: now_ms,
            ended_ms: None,
            output: VecDeque::new(),
            dropped_lines: 0,
            lines_total: 0,
            hash,
            composed: composed.clone(),
        };
        let request = SpawnRequest {
            id,
            source: composed.text,
        };
        self.markers.push(RunMarker {
            id,
            hash,
            status: None,
        });
        let spawned = self.runner.spawn(&request);
        if let Err(reason) = &spawned {
            run.state = RunState::Failed(reason.clone());
            run.ended_ms = Some(now_ms);
            self.markers.push(RunMarker {
                id,
                hash,
                status: Some("failed".to_string()),
            });
        }
        self.runs.push(run);
        self.forget_old();
        spawned.map(|()| id)
    }

    fn forget_old(&mut self) {
        let finished = self.runs.iter().filter(|r| !r.state.is_live()).count();
        let mut excess = finished.saturating_sub(MAX_FINISHED);
        self.runs.retain(|r| {
            if excess > 0 && !r.state.is_live() {
                excess -= 1;
                false
            } else {
                true
            }
        });
    }

    /// Ask run `id` to end. It becomes `Stopped` when the child is gone.
    pub fn stop(&mut self, id: RunId) {
        if let Some(run) = self.runs.iter_mut().find(|r| r.id == id) {
            if run.state == RunState::Running {
                run.state = RunState::Stopping;
                self.runner.stop(id);
            }
        }
    }

    /// Stop every live run (the window is closing).
    pub fn stop_all(&mut self) {
        let live: Vec<RunId> = self
            .runs
            .iter()
            .filter(|r| r.state == RunState::Running)
            .map(|r| r.id)
            .collect();
        for id in live {
            self.stop(id);
        }
    }

    /// Take what the children did since the last call.
    pub fn poll(&mut self, now_ms: u64) {
        let mut events = Vec::new();
        self.runner.poll(&mut events);
        for event in events {
            match event {
                RunnerEvent::Output { id, stream, line } => self.output(id, stream, line),
                RunnerEvent::Exit { id, code } => self.exit(id, code, now_ms),
            }
        }
    }

    fn output(&mut self, id: RunId, stream: Stream, line: String) {
        let Some(run) = self.runs.iter_mut().find(|r| r.id == id) else {
            return;
        };
        let text: String = line.chars().take(MAX_LINE).collect();
        let mark = (stream == Stream::Stderr)
            .then(|| run.composed.map_error(&text))
            .flatten();
        run.lines_total += 1;
        if run.output.len() >= MAX_OUTPUT_LINES {
            run.output.pop_front();
            run.dropped_lines += 1;
        }
        run.output.push_back(OutputLine { stream, text, mark });
    }

    fn exit(&mut self, id: RunId, code: Option<i32>, now_ms: u64) {
        let Some(run) = self.runs.iter_mut().find(|r| r.id == id) else {
            return;
        };
        if !run.state.is_live() {
            return;
        }
        run.state = match (run.state.clone(), code) {
            (RunState::Stopping, _) | (_, None) => RunState::Stopped,
            (_, Some(code)) => RunState::Exited(code),
        };
        run.ended_ms = Some(now_ms);
        self.markers.push(RunMarker {
            id,
            hash: run.hash,
            status: run.state.status_text(),
        });
        self.forget_old();
    }

    /// The markers to trace, oldest first; the table forgets them.
    pub fn take_markers(&mut self) -> Vec<RunMarker> {
        std::mem::take(&mut self.markers)
    }

    pub fn get(&self, id: RunId) -> Option<&Run> {
        self.runs.iter().find(|r| r.id == id)
    }

    pub fn runs(&self) -> &[Run] {
        &self.runs
    }

    pub fn live_count(&self) -> usize {
        self.runs.iter().filter(|r| r.state.is_live()).count()
    }
}
