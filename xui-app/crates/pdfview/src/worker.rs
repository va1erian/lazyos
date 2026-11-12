//! The render threads. The window hands the pool a list of jobs, nearest
//! first, which replaces whatever was still waiting (a scroll or a zoom makes
//! the old list stale); the threads take jobs from the front, render them
//! with their own [`lazypdf::Renderer`], and post the pixels back through a
//! [`Sink`] as an app message (`xui-work`), so the window wakes for each tile
//! and nothing polls.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use lazypdf::{Document, Renderer, Tile};
use xui_work::Sink;

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
#[derive(Clone)]
pub struct Done {
    /// The document generation the job belonged to ([`Pool::new`]), so a tile
    /// still in flight from a closed document is not shown in the next one.
    pub epoch: u64,
    pub key: Key,
    /// `None` when the page could not be drawn.
    pub tile: Option<Tile>,
    pub millis: f32,
}

impl std::fmt::Debug for Done {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Done")
            .field("epoch", &self.epoch)
            .field("key", &self.key)
            .field("drawn", &self.tile.is_some())
            .field("millis", &self.millis)
            .finish()
    }
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
    started: bool,
}

impl Pool {
    /// Starts `threads` render threads over `doc`; each finished job goes to
    /// `results`, stamped `epoch`.
    pub fn new(doc: Arc<Document>, threads: usize, epoch: u64, results: Sink<Done>) -> Pool {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
        });
        let mut started = false;
        for n in 0..threads.max(1) {
            let (doc, shared, tx) = (Arc::clone(&doc), Arc::clone(&shared), results.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("pdf-render-{n}"))
                .spawn(move || work(&doc, &shared, epoch, &tx));
            started |= spawned.is_ok();
        }
        Pool { shared, started }
    }

    /// Whether at least one render thread started. Without one nothing would
    /// ever draw; the window shows blank pages rather than hang, and the
    /// viewer says why (`PDF:WORKER:FAIL`).
    pub fn started(&self) -> bool {
        self.started
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

fn work(doc: &Document, shared: &Shared, epoch: u64, tx: &Sink<Done>) {
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
            epoch,
            key: job.key,
            tile,
            millis: started.elapsed().as_secs_f32() * 1000.0,
        };
        // Post first, then clear `running`: the window never sees the key as
        // neither pending nor drawn, so it never asks for it twice.
        let sent = tx.send(done);
        lock(&shared.queue).running.remove(&job.key);
        if !sent {
            return;
        }
    }
}
