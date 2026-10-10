//! The render threads. The window hands the pool a list of jobs, nearest
//! first, which replaces whatever was still waiting (a scroll or a zoom makes
//! the old list stale); the threads take jobs from the front, render them
//! with their own [`lazypdf::Renderer`], and post the pixels back. The window
//! drains the results from a timer. That predates the backend's waker
//! (`xui-app/src/backend/wakeup.rs`): a `Proxy` would now do.

use std::collections::{HashSet, VecDeque};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use lazypdf::{Document, Renderer, Tile};

use crate::cache::Key;

/// One rectangle of one page to draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Job {
    pub key: Key,
    pub page: usize,
    /// Pixels per point.
    pub scale: f32,
    /// The rectangle in the page's pixels at `scale`.
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// A finished job.
pub struct Done {
    pub key: Key,
    /// `None` when the page could not be drawn.
    pub tile: Option<Tile>,
    pub millis: f32,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    /// Keys a thread is drawing now, so a new list does not ask again.
    running: HashSet<Key>,
    quit: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

/// The threads drawing one document.
pub struct Pool {
    shared: Arc<Shared>,
    results: Receiver<Done>,
}

impl Pool {
    /// Starts `threads` render threads over `doc`.
    pub fn new(doc: Arc<Document>, threads: usize) -> Pool {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
        });
        let (tx, results) = channel();
        for n in 0..threads.max(1) {
            let (doc, shared, tx) = (Arc::clone(&doc), Arc::clone(&shared), tx.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("pdf-render-{n}"))
                .spawn(move || work(&doc, &shared, &tx));
            if spawned.is_err() && n == 0 {
                // Without a single thread nothing would ever draw; the window
                // shows blank pages rather than hang, and says why.
                eprintln!("PDF:WORKER:FAIL");
            }
        }
        Pool { shared, results }
    }

    /// How many threads suit this machine: all but one CPU, at most four.
    pub fn default_threads() -> usize {
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        cpus.saturating_sub(1).clamp(1, 4)
    }

    /// Replaces the waiting jobs with `jobs` (minus any a thread is drawing).
    pub fn set_jobs(&self, jobs: Vec<Job>) {
        let mut queue = lock(&self.shared.queue);
        let running = std::mem::take(&mut queue.running);
        queue.jobs = jobs
            .into_iter()
            .filter(|j| !running.contains(&j.key))
            .collect();
        queue.running = running;
        drop(queue);
        self.shared.wake.notify_all();
    }

    /// Whether any job is waiting or running.
    pub fn busy(&self) -> bool {
        let queue = lock(&self.shared.queue);
        !queue.jobs.is_empty() || !queue.running.is_empty()
    }

    /// The next finished job, if one is ready.
    pub fn try_recv(&self) -> Option<Done> {
        // Empty and disconnected (every thread gone) both mean nothing new.
        self.results.try_recv().ok()
    }
}

impl Drop for Pool {
    /// Tells the threads to stop after their current job. They are not
    /// joined: a page can take a while and the window must not wait for it.
    fn drop(&mut self) {
        let mut queue = lock(&self.shared.queue);
        queue.quit = true;
        queue.jobs.clear();
        drop(queue);
        self.shared.wake.notify_all();
    }
}

fn lock(queue: &Mutex<Queue>) -> std::sync::MutexGuard<'_, Queue> {
    // A render thread never panics while holding the lock (the image is
    // `panic = "abort"` anyway), so a poisoned lock still holds sound data.
    queue
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn work(doc: &Document, shared: &Shared, tx: &Sender<Done>) {
    let renderer = Renderer::new(doc);
    loop {
        let job = {
            let mut queue = lock(&shared.queue);
            loop {
                if queue.quit {
                    return;
                }
                if let Some(job) = queue.jobs.pop_front() {
                    queue.running.insert(job.key);
                    break job;
                }
                queue = shared.wake.wait(queue).unwrap_or_else(|p| p.into_inner());
            }
        };
        let started = Instant::now();
        let tile = renderer.render_tile(job.page, job.scale, job.x, job.y, job.w, job.h);
        let done = Done {
            key: job.key,
            tile,
            millis: started.elapsed().as_secs_f32() * 1000.0,
        };
        // Post first, then clear `running`: the window never sees the key as
        // neither pending nor drawn, so it never asks for it twice.
        let sent = tx.send(done).is_ok();
        lock(&shared.queue).running.remove(&job.key);
        if !sent {
            return;
        }
    }
}
