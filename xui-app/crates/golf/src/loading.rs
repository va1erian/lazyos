//! The course generator on a worker thread, so the window keeps painting and
//! answering input while a slow CPU grinds through the search. Progress and
//! the finished course reach the window as messages (`xui-work`); nothing
//! polls for them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use xui_work::{Job, Report, Sink};

use crate::course::Course;
use crate::gen::{Generator, Params};

/// How often the generator tells the window how far it got.
const PROGRESS_GAP: Duration = Duration::from_millis(100);

/// A finished course in a message, shared rather than copied.
#[derive(Clone, Debug)]
pub struct Shared(pub Arc<Course>);

impl PartialEq for Shared {
    fn eq(&self, other: &Shared) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// What a generation tells the window. `seq` names the generation, so a
/// message from a superseded search is ignored.
#[derive(Clone, Debug, PartialEq)]
pub enum Gen {
    Progress {
        seq: u64,
        label: String,
        done: f32,
    },
    Ready {
        seq: u64,
        course: Shared,
    },
    /// The search was cancelled before it found a course.
    Stopped {
        seq: u64,
    },
    /// The worker could not run.
    Failed {
        seq: u64,
    },
}

impl Gen {
    fn seq(&self) -> u64 {
        match self {
            Gen::Progress { seq, .. }
            | Gen::Ready { seq, .. }
            | Gen::Stopped { seq }
            | Gen::Failed { seq } => *seq,
        }
    }
}

/// A course being generated on a worker thread, so the window keeps
/// painting and answering input while a slow CPU grinds through the search.
/// Dropping it cancels the search at its next step.
pub struct Loading {
    seq: u64,
    _job: Job,
    progress: (String, f32),
    /// The course, once it arrived, until the game takes it.
    course: Option<Arc<Course>>,
    pub started: Instant,
    /// The progress the last tick showed, to repaint only on a change.
    pub shown: (String, f32),
    pub failed: bool,
}

impl Loading {
    /// Starts generation `seq`; its messages go to `to`.
    pub fn start(params: Params, seq: u64, to: &Sink<Gen>) -> Loading {
        let job = xui_work::spawn_with(
            to,
            "golf-gen",
            move |report: &Report<(String, f32)>| {
                let mut generator = Generator::new(params);
                while !report.cancelled() {
                    report.paced(PROGRESS_GAP, || generator.progress());
                    if let Some(course) = generator.step() {
                        return Some(course);
                    }
                }
                None
            },
            move |(label, done)| Gen::Progress { seq, label, done },
            move |result| match result {
                Ok(Some(course)) => Gen::Ready {
                    seq,
                    course: Shared(Arc::new(course)),
                },
                Ok(None) => Gen::Stopped { seq },
                Err(_) => Gen::Failed { seq },
            },
        );
        Loading {
            seq,
            _job: job,
            progress: Generator::new(params).progress(),
            course: None,
            started: Instant::now(),
            shown: (String::new(), -1.0),
            failed: false,
        }
    }

    /// Takes in a message of this generation; others are dropped.
    pub fn accept(&mut self, message: Gen) {
        if message.seq() != self.seq {
            return;
        }
        match message {
            Gen::Progress { label, done, .. } => self.progress = (label, done),
            Gen::Ready { course, .. } => self.course = Some(course.0),
            Gen::Stopped { .. } | Gen::Failed { .. } => self.failed = true,
        }
    }

    /// The course, once, when it has arrived.
    pub fn take_course(&mut self) -> Option<Arc<Course>> {
        self.course.take()
    }

    pub fn snapshot(&self) -> (String, f32) {
        if self.failed {
            return ("Generation failed".into(), 0.0);
        }
        self.progress.clone()
    }
}
