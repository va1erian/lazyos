//! A fake child process for the run table's tests (and for the window's
//! offscreen tests): each spawn takes the next scripted [`Plan`].

use std::collections::VecDeque;

use crate::runner::{RunId, Runner, RunnerEvent, SpawnRequest, Stream};

/// What the next child does.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// Output lines, in order.
    pub output: Vec<(Stream, String)>,
    /// The exit code once the output is out; `None` never exits by itself
    /// (a server script), only Stop ends it.
    pub exit: Option<i32>,
    /// `spawn` fails with this reason.
    pub refuse: Option<String>,
    /// Lines released per `poll` call (0 means all).
    pub per_poll: usize,
}

impl Plan {
    pub fn lines(lines: &[&str], exit: i32) -> Plan {
        Plan {
            output: lines
                .iter()
                .map(|l| (Stream::Stdout, (*l).to_string()))
                .collect(),
            exit: Some(exit),
            ..Plan::default()
        }
    }

    /// A child that prints `lines` on stderr and exits 1.
    pub fn error(lines: &[&str]) -> Plan {
        Plan {
            output: lines
                .iter()
                .map(|l| (Stream::Stderr, (*l).to_string()))
                .collect(),
            exit: Some(1),
            ..Plan::default()
        }
    }

    /// A child that never ends by itself.
    pub fn forever(lines: &[&str]) -> Plan {
        Plan {
            exit: None,
            ..Plan::lines(lines, 0)
        }
    }
}

struct Child {
    id: RunId,
    pending: VecDeque<(Stream, String)>,
    exit: Option<i32>,
    per_poll: usize,
    killed: bool,
    done: bool,
}

/// The fake. Plans are consumed in the order children are spawned.
#[derive(Default)]
pub struct FakeRunner {
    plans: VecDeque<Plan>,
    children: Vec<Child>,
    /// Every request, in order.
    pub spawned: Vec<SpawnRequest>,
    /// Every `stop` call.
    pub stopped: Vec<RunId>,
}

impl FakeRunner {
    pub fn new() -> FakeRunner {
        FakeRunner::default()
    }

    pub fn with_plans(plans: impl IntoIterator<Item = Plan>) -> FakeRunner {
        FakeRunner {
            plans: plans.into_iter().collect(),
            ..FakeRunner::default()
        }
    }

    pub fn push_plan(&mut self, plan: Plan) {
        self.plans.push_back(plan);
    }

    /// Children that have not ended.
    pub fn alive(&self) -> usize {
        self.children.iter().filter(|c| !c.done).count()
    }
}

impl Runner for FakeRunner {
    fn spawn(&mut self, request: &SpawnRequest) -> Result<(), String> {
        self.spawned.push(request.clone());
        let plan = self
            .plans
            .pop_front()
            .unwrap_or_else(|| Plan::lines(&[], 0));
        if let Some(reason) = plan.refuse {
            return Err(reason);
        }
        self.children.push(Child {
            id: request.id,
            pending: plan.output.into(),
            exit: plan.exit,
            per_poll: plan.per_poll,
            killed: false,
            done: false,
        });
        Ok(())
    }

    fn poll(&mut self, out: &mut Vec<RunnerEvent>) {
        for child in self.children.iter_mut().filter(|c| !c.done) {
            if child.killed {
                child.done = true;
                out.push(RunnerEvent::Exit {
                    id: child.id,
                    code: None,
                });
                continue;
            }
            let take = if child.per_poll == 0 {
                child.pending.len()
            } else {
                child.per_poll.min(child.pending.len())
            };
            for _ in 0..take {
                if let Some((stream, line)) = child.pending.pop_front() {
                    out.push(RunnerEvent::Output {
                        id: child.id,
                        stream,
                        line,
                    });
                }
            }
            if child.pending.is_empty() {
                if let Some(code) = child.exit {
                    child.done = true;
                    out.push(RunnerEvent::Exit {
                        id: child.id,
                        code: Some(code),
                    });
                }
            }
        }
    }

    fn stop(&mut self, id: RunId) {
        self.stopped.push(id);
        if let Some(child) = self.children.iter_mut().find(|c| c.id == id) {
            child.killed = true;
        }
    }
}
