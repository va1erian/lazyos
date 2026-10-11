//! The layout thread (docs/lazysky-plan.md §7.1, R6): shapes rows off the UI
//! thread, nearest first, and drops stale work.
//!
//! Two queues. *Urgent* jobs are the rows of a page about to enter the feed;
//! none may be lost. *Refresh* jobs re-prepare rows that are on screen or
//! near it (a width change, a row that came back from dehydration); a new
//! list replaces what still waits. Urgent jobs always run first.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

use lazysky_core::feed::RowId;
use lazysky_core::proto::FeedItem;
use xui_core::backend::TextShaper;

use crate::prepare::{prepare, prepare_load_more, Context, Prepared};

pub enum Work {
    Post(Arc<FeedItem>),
    LoadMore,
}

pub struct Job {
    pub id: RowId,
    pub work: Work,
    pub ctx: Context,
    /// Which batch an urgent job belongs to (the page's request number).
    pub tag: u64,
}

pub struct Done {
    pub id: RowId,
    pub prepared: Prepared,
    pub tag: u64,
    pub urgent: bool,
}

#[derive(Default)]
struct Queues {
    urgent: VecDeque<Job>,
    refresh: VecDeque<Job>,
    closed: bool,
}

struct Shared {
    queues: Mutex<Queues>,
    wake: Condvar,
    /// Jobs taken off a queue and not yet answered.
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
            queues: Mutex::new(Queues::default()),
            wake: Condvar::new(),
            running: AtomicUsize::new(0),
        });
        let (tx, results) = mpsc::channel();
        let worker = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("sky-layout".into())
            .spawn(move || run(&*shaper, &worker, &tx))
            .expect("the layout thread starts");
        Layouts {
            shared,
            results,
            thread: Some(thread),
        }
    }

    /// Queues rows that must be prepared (a page's).
    pub fn submit(&self, jobs: Vec<Job>) {
        let mut queues = self.lock();
        queues.urgent.extend(jobs);
        drop(queues);
        self.shared.wake.notify_one();
    }

    /// Replaces the refresh list (nearest first).
    pub fn set_refresh(&self, jobs: Vec<Job>) {
        let mut queues = self.lock();
        queues.refresh = jobs.into();
        drop(queues);
        self.shared.wake.notify_one();
    }

    /// A finished row, if one is waiting.
    pub fn try_next(&self) -> Option<Done> {
        self.results.try_recv().ok()
    }

    /// Jobs queued or running.
    pub fn pending(&self) -> usize {
        let queues = self.lock();
        queues.urgent.len() + queues.refresh.len() + self.shared.running.load(Ordering::SeqCst)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Queues> {
        self.shared
            .queues
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
        let (job, urgent) = {
            let mut queues = shared.queues.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if queues.closed {
                    return;
                }
                if let Some(job) = queues.urgent.pop_front() {
                    shared.running.fetch_add(1, Ordering::SeqCst);
                    break (job, true);
                }
                if let Some(job) = queues.refresh.pop_front() {
                    shared.running.fetch_add(1, Ordering::SeqCst);
                    break (job, false);
                }
                queues = shared
                    .wake
                    .wait(queues)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let prepared = match &job.work {
            Work::Post(item) => prepare(shaper, item, job.ctx),
            Work::LoadMore => prepare_load_more(shaper, job.ctx),
        };
        let sent = results.send(Done {
            id: job.id,
            prepared,
            tag: job.tag,
            urgent,
        });
        shared.running.fetch_sub(1, Ordering::SeqCst);
        if sent.is_err() {
            return;
        }
    }
}
