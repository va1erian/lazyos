//! The layout thread (docs/ai-agent-plan.md §2.1): parses Markdown and shapes
//! rows off the UI thread. One queue; a new job for a row replaces the one
//! still waiting for it (a streaming reply is laid out again for each new
//! batch of text, and only the newest matters).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};

use xui_core::backend::TextShaper;

use crate::model::{RowId, RowSpec};
use crate::prepare::{prepare, Context, Prepared};

pub struct Job {
    pub id: RowId,
    pub spec: Arc<RowSpec>,
    pub ctx: Context,
    /// Which version of the row this is; echoed in the answer.
    pub rev: u64,
}

pub struct Done {
    pub id: RowId,
    pub rev: u64,
    /// The text that was shaped: the row may have outgrown it.
    pub spec: Arc<RowSpec>,
    pub prepared: Prepared,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    closed: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    running: AtomicUsize,
}

pub struct Layouts {
    shared: Arc<Shared>,
    results: Receiver<Done>,
    thread: Option<JoinHandle<()>>,
}

impl Layouts {
    /// Starts the thread with `shaper` (taken from the UI thread once).
    pub fn start(shaper: Box<dyn TextShaper>) -> Layouts {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            running: AtomicUsize::new(0),
        });
        let (tx, results) = mpsc::channel();
        let worker = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("agent-layout".into())
            .spawn(move || run(&*shaper, &worker, &tx))
            .expect("the layout thread starts");
        Layouts {
            shared,
            results,
            thread: Some(thread),
        }
    }

    /// Queues `jobs`; a row's older waiting job is dropped.
    pub fn submit(&self, jobs: Vec<Job>) {
        let mut queue = self.lock();
        for job in jobs {
            queue.jobs.retain(|waiting| waiting.id != job.id);
            queue.jobs.push_back(job);
        }
        drop(queue);
        self.shared.wake.notify_one();
    }

    /// Drops every waiting job (a new conversation replaced the rows).
    pub fn clear(&self) {
        self.lock().jobs.clear();
    }

    pub fn try_next(&self) -> Option<Done> {
        self.results.try_recv().ok()
    }

    /// Jobs queued or running.
    pub fn pending(&self) -> usize {
        self.lock().jobs.len() + self.shared.running.load(Ordering::SeqCst)
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for Layouts {
    fn drop(&mut self) {
        self.lock().closed = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(shaper: &dyn TextShaper, shared: &Shared, results: &Sender<Done>) {
    loop {
        let job = {
            let mut queue = shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if queue.closed {
                    return;
                }
                if let Some(job) = queue.jobs.pop_front() {
                    shared.running.fetch_add(1, Ordering::SeqCst);
                    break job;
                }
                queue = shared
                    .wake
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let prepared = prepare(shaper, &job.spec, job.ctx);
        let sent = results.send(Done {
            id: job.id,
            rev: job.rev,
            spec: job.spec,
            prepared,
        });
        shared.running.fetch_sub(1, Ordering::SeqCst);
        if sent.is_err() {
            return;
        }
    }
}
