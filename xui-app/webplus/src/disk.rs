//! The window's own files written off the window's thread: the browsing history (`visits.rs`) and the
//! session kept for a crash (`session.rs`).
//!
//! The window's thread does no I/O (AGENTS.md, "Performance"). On LazyOS a file call can wait on
//! the volume's lock for as long as another program's `fsync` or `rename` holds it (hundreds of
//! milliseconds while the engine's processes write their cache, `lazywebplus-performance-plan.md`
//! section 11), and the session file's own write syncs twice. Both were written on the window's thread
//! after every load and every title a script set: the page stopped scrolling and taking clicks
//! for as long. Here a write is a job queued for one thread, run in the order queued (a history
//! append, then the rewrite after it). A job with a key replaces one of the same key still waiting:
//! only the newest session matters. The thread starts with the first job, parks on a condition
//! variable while there is nothing to write (no timer) and lives as long as the process.

use std::collections::VecDeque;
use std::io;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

type Job = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct State {
    /// Jobs in the order queued, with the key of a job only the newest of matters.
    jobs: VecDeque<(Option<&'static str>, Job)>,
    /// A job is being run.
    busy: bool,
    /// The thread runs (or could not be started: jobs then run on the queuing thread).
    started: bool,
    inline: bool,
}

/// Writes files on a thread of its own, in the order asked.
#[derive(Clone, Default)]
pub struct Disk {
    shared: Arc<(Mutex<State>, Condvar)>,
}

/// The process's writer.
pub fn global() -> &'static Disk {
    static DISK: OnceLock<Disk> = OnceLock::new();
    DISK.get_or_init(Disk::default)
}

impl Disk {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `job` after every job queued before it.
    pub fn run(&self, job: impl FnOnce() + Send + 'static) {
        self.queue(None, Box::new(job));
    }

    /// Runs `job` like [`Disk::run`], dropping a job of the same `key` that has not started yet.
    pub fn replace(&self, key: &'static str, job: impl FnOnce() + Send + 'static) {
        self.queue(Some(key), Box::new(job));
    }

    fn queue(&self, key: Option<&'static str>, job: Job) {
        let mut state = self.lock();
        if state.inline {
            drop(state);
            job();
            return;
        }
        if key.is_some() {
            state.jobs.retain(|(waiting, _)| *waiting != key);
        }
        state.jobs.push_back((key, job));
        let start = !std::mem::replace(&mut state.started, true);
        drop(state);
        self.shared.1.notify_all();
        if start {
            self.start();
        }
    }

    fn start(&self) {
        let disk = self.clone();
        let spawned = thread::Builder::new()
            .name(String::from("webplus-disk"))
            .spawn(move || disk.serve());
        if spawned.is_err() {
            // No thread (task_max, memory): what is queued, and every later job, runs on the caller.
            let waiting: Vec<Job> = {
                let mut state = self.lock();
                state.inline = true;
                state.jobs.drain(..).map(|(_, job)| job).collect()
            };
            waiting.into_iter().for_each(|job| job());
            self.shared.1.notify_all();
        }
    }

    /// The thread: run each job as it comes, park while there is none.
    fn serve(&self) {
        let mut state = self.lock();
        loop {
            match state.jobs.pop_front() {
                Some((_, job)) => {
                    state.busy = true;
                    drop(state);
                    // Jobs report their own errors; the program is built with panic = "abort".
                    job();
                    state = self.lock();
                    state.busy = false;
                    self.shared.1.notify_all();
                }
                None => {
                    state = self
                        .shared
                        .1
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
        }
    }

    /// Runs `job` after every job queued before it and waits up to `limit` for its answer: for a
    /// change the person asked for and must hear the outcome of (clearing the history). `TimedOut`
    /// when the disk is too slow to answer in time: the job still runs, and reports a failure itself
    /// when nobody waits for it any more.
    pub fn call(
        &self,
        limit: Duration,
        job: impl FnOnce() -> io::Result<()> + Send + 'static,
    ) -> io::Result<()> {
        let (tx, rx) = mpsc::channel();
        self.run(move || {
            if let Err(mpsc::SendError(Err(e))) = tx.send(job()) {
                eprintln!("lazywebplus: a write finished late and failed: {e}");
            }
        });
        rx.recv_timeout(limit).unwrap_or_else(|_| {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the disk is busy, still writing",
            ))
        })
    }

    /// Waits up to `limit` for every job queued so far (the window closed, the process exits next).
    /// True when nothing is left to write.
    pub fn flush(&self, limit: Duration) -> bool {
        let until = Instant::now() + limit;
        let mut state = self.lock();
        while state.busy || !state.jobs.is_empty() {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self
                .shared
                .1
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_run_off_the_callers_thread_in_order() {
        let disk = Disk::default();
        let caller = thread::current().id();
        let (tx, rx) = mpsc::channel();
        for n in 0..50 {
            let tx = tx.clone();
            disk.run(move || tx.send((n, thread::current().id())).unwrap());
        }
        assert!(disk.flush(Duration::from_secs(5)));
        let seen: Vec<_> = rx.try_iter().collect();
        assert_eq!(
            seen.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            (0..50).collect::<Vec<_>>()
        );
        assert!(seen.iter().all(|(_, id)| *id != caller));
    }

    #[test]
    fn a_keyed_job_replaces_the_one_still_waiting() {
        let disk = Disk::default();
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let (tx, rx) = mpsc::channel();
        // Hold the thread so the next jobs wait.
        disk.run(move || gate_rx.recv().unwrap());
        for n in 0..5 {
            let tx = tx.clone();
            disk.replace("session", move || tx.send(n).unwrap());
        }
        let ordered = tx.clone();
        disk.run(move || ordered.send(100).unwrap());
        gate_tx.send(()).unwrap();
        assert!(disk.flush(Duration::from_secs(5)));
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![4, 100]);
    }

    #[test]
    fn a_call_waits_for_its_answer_after_what_was_queued_before() {
        let disk = Disk::default();
        let (tx, rx) = mpsc::channel();
        disk.run(move || tx.send(1).unwrap());
        let failed = disk.call(Duration::from_secs(5), || Err(io::Error::other("full")));
        assert_eq!(failed.unwrap_err().to_string(), "full");
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![1]);
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        disk.run(move || gate_rx.recv().unwrap());
        let slow = disk.call(Duration::from_millis(20), || Ok(()));
        assert_eq!(slow.unwrap_err().kind(), io::ErrorKind::TimedOut);
        gate_tx.send(()).unwrap();
        assert!(disk.flush(Duration::from_secs(5)));
    }

    #[test]
    fn a_flush_that_runs_out_of_time_says_so() {
        let disk = Disk::default();
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        disk.run(move || gate_rx.recv().unwrap());
        assert!(!disk.flush(Duration::from_millis(20)));
        gate_tx.send(()).unwrap();
        assert!(disk.flush(Duration::from_secs(5)));
    }
}
