//! The save threads: each takes one picture job at a time, fetches it with
//! its own HTTP client and writes it whole or not at all.

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::bulk::BulkState;
use super::part_path;
use crate::net::{Http, HttpOptions};
use crate::{Event, Platform};

/// Concurrent downloads (a bulk run uses all of them).
pub const THREADS: usize = 3;
/// The most a picture's bytes may be.
const MAX_BODY: usize = 32 * 1024 * 1024;

/// One picture to fetch and write.
pub struct SaveJob {
    pub seq: u64,
    pub url: String,
    pub path: PathBuf,
    /// Set for a bulk download: the job reports into it.
    pub bulk: Option<Arc<BulkState>>,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<SaveJob>,
    closed: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

pub struct SavePool {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl SavePool {
    pub fn start(platform: Arc<dyn Platform>, events: Sender<Event>) -> SavePool {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
        });
        let threads = (0..THREADS)
            .map(|n| {
                let (shared, platform, events) =
                    (Arc::clone(&shared), Arc::clone(&platform), events.clone());
                thread::Builder::new()
                    .name(format!("sky-save{n}"))
                    .spawn(move || run(&shared, &*platform, &events))
                    .expect("a save thread starts")
            })
            .collect();
        SavePool { shared, threads }
    }

    pub fn submit(&self, job: SaveJob) {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .jobs
            .push_back(job);
        self.shared.wake.notify_one();
    }
}

impl Drop for SavePool {
    fn drop(&mut self) {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed = true;
        self.shared.wake.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn run(shared: &Shared, platform: &dyn Platform, events: &Sender<Event>) {
    let http = Http::new(
        platform.ca_roots(),
        HttpOptions {
            max_body: MAX_BODY,
            read_timeout: Duration::from_secs(60),
            ..crate::net::http_options()
        },
    );
    loop {
        let job = {
            let mut queue = shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if queue.closed {
                    return;
                }
                if let Some(job) = queue.jobs.pop_front() {
                    break job;
                }
                queue = shared
                    .wake
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        if !handle(&http, platform, events, job) {
            return;
        }
    }
}

/// Runs one job; `false` when the receiver is gone.
fn handle(http: &Http, platform: &dyn Platform, events: &Sender<Event>, job: SaveJob) -> bool {
    match &job.bulk {
        Some(state) => {
            let outcome = if state.cancel.load(Ordering::SeqCst) {
                Outcome::Cancelled
            } else if job.path.exists() {
                Outcome::Skipped
            } else {
                match fetch_and_write(http, &job) {
                    Ok(_) => Outcome::Saved,
                    Err(why) => {
                        platform.log(&format!("SKY:BULK:FAIL:{why}"));
                        Outcome::Failed
                    }
                }
            };
            for event in state.finished(outcome) {
                if events.send(event).is_err() {
                    return false;
                }
            }
            true
        }
        None => {
            let event = match fetch_and_write(http, &job) {
                Ok(bytes) => {
                    platform.log(&format!("SKY:SAVE:DONE:{}:{bytes}", job.path.display()));
                    Event::Saved {
                        seq: job.seq,
                        path: job.path.clone(),
                        bytes,
                    }
                }
                Err(reason) => {
                    platform.log(&format!("SKY:SAVE:FAIL:{reason}"));
                    Event::Failed {
                        seq: job.seq,
                        reason,
                        signed_out: false,
                    }
                }
            };
            events.send(event).is_ok()
        }
    }
}

/// What happened to one bulk job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Saved,
    Skipped,
    Failed,
    Cancelled,
}

/// Fetches `job.url` and writes it to `job.path` through `<path>.part`.
/// Returns the byte count.
fn fetch_and_write(http: &Http, job: &SaveJob) -> Result<u64, String> {
    let response = http.get(&job.url, &[("accept", "image/*")])?;
    if response.status != 200 {
        return Err(format!("the picture server answered {}", response.status));
    }
    if let Some(dir) = job.path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let part = part_path(&job.path);
    let write = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(&part)?;
        file.write_all(&response.body)?;
        file.flush()?;
        file.sync_all()?;
        std::fs::rename(&part, &job.path)
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&part);
        return Err(format!("{}: {e}", job.path.display()));
    }
    Ok(response.body.len() as u64)
}
