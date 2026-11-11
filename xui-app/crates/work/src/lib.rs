//! Work off the UI thread, answered as messages.
//!
//! The UI thread does no I/O (AGENTS.md), and it never polls a timer for a
//! result: a worker hands its answer to the window as an ordinary app
//! message. xui's [`Proxy`] pushes onto the window's inbox and posts one wake
//! through the backend's waker (`xui-app/src/backend/wakeup.rs`), so a parked
//! loop answers at once and an idle one stays parked.
//!
//! * [`spawn`]: run `work` on a named thread, deliver `done(result)`.
//! * [`spawn_with`]: the same with progress messages and cancellation; the
//!   returned [`Job`] cancels on drop.
//! * [`Queue`]: one long-lived worker serving requests in order.
//! * [`inbox`]: a queue of small results the UI drains in batches, behind one
//!   doorbell message.
//! * [`Sink`]: a handle that posts values of one type, for a crate that does
//!   not name the app's message type (a pool of render threads, a doorbell).
//!
//! A worker that cannot start or that panics still answers, with
//! [`Failed`]; nothing waits for a message that cannot come. (The OS image is
//! built with `panic = "abort"`, where a panic ends the process instead; the
//! host tests and the desktop runner get the message.)

mod inbox;
mod queue;
mod sink;

use std::cell::Cell;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use inbox::{inbox, inbox_with, Inbox};
pub use queue::Queue;
pub use sink::{Mailbox, Sink};

/// Why a worker gave no result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failed {
    /// The worker's thread name.
    pub worker: String,
    /// Why: the OS error that stopped the thread starting, or the panic text.
    pub reason: String,
}

impl fmt::Display for Failed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "worker {} failed: {}", self.worker, self.reason)
    }
}

impl std::error::Error for Failed {}

/// The text of a caught panic.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panicked".to_owned())
}

fn take<T>(slot: &Mutex<Option<T>>) -> Option<T> {
    slot.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Runs `work` on a thread called `name` and posts `done(result)` to `to`
/// (a window's `ui.proxy()` or a [`Sink`]).
///
/// `done` also hears a failure to start the thread or a panic in `work`, as
/// [`Failed`], so the caller never waits for an answer that cannot come.
pub fn spawn<M, R, B>(
    to: &B,
    name: &str,
    work: impl FnOnce() -> R + Send + 'static,
    done: impl FnOnce(Result<R, Failed>) -> M + Send + 'static,
) where
    B: Mailbox<M>,
    R: 'static,
    M: 'static,
{
    // Both halves wait in a slot the thread takes them from, so a thread that
    // never starts leaves `done` here to report the failure with.
    let parts = Arc::new(Mutex::new(Some((work, done))));
    let (thread_parts, mailbox, who) = (Arc::clone(&parts), to.clone(), name.to_owned());
    let started = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let Some((work, done)) = take(&thread_parts) else {
                return;
            };
            let result = catch_unwind(AssertUnwindSafe(work)).map_err(|payload| Failed {
                worker: who,
                reason: panic_text(&*payload),
            });
            mailbox.post(done(result));
        });
    if let Err(error) = started {
        if let Some((_, done)) = take(&parts) {
            to.post(done(Err(Failed {
                worker: name.to_owned(),
                reason: format!("could not start the thread: {error}"),
            })));
        }
    }
}

/// The worker's side of a [`spawn_with`] job: where it reports progress and
/// learns it should stop.
pub struct Report<P> {
    post: Box<dyn Fn(P) + Send>,
    cancel: Arc<AtomicBool>,
    last: Cell<Option<Instant>>,
}

impl<P> Report<P> {
    /// Posts a progress message. Every call posts; a hot loop uses
    /// [`paced`](Report::paced).
    pub fn report(&self, progress: P) {
        (self.post)(progress);
    }

    /// Posts `progress` unless one went out less than `gap` ago; whether it
    /// did. The UI repaints per message, so a worker looping thousands of
    /// times a second paces its reports.
    pub fn paced(&self, gap: Duration, progress: impl FnOnce() -> P) -> bool {
        let now = Instant::now();
        if self
            .last
            .get()
            .is_some_and(|at| now.duration_since(at) < gap)
        {
            return false;
        }
        self.last.set(Some(now));
        (self.post)(progress());
        true
    }

    /// Whether the job was cancelled (or its handle dropped): stop soon and
    /// return what there is.
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// A running job started by [`spawn_with`]. Dropping it cancels, so a window
/// that forgets its job stops the work; [`detach`](Job::detach) lets the work
/// finish regardless.
pub struct Job {
    cancel: Arc<AtomicBool>,
    detached: bool,
}

impl Job {
    /// Asks the worker to stop. Its `done` still arrives, carrying whatever
    /// the worker returned.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Whether the job was cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Lets the work run on after this handle is gone.
    pub fn detach(mut self) {
        self.detached = true;
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if !self.detached {
            self.cancel();
        }
    }
}

/// Like [`spawn`], with progress and cancellation: `work` gets a [`Report`]
/// that posts `progress(value)` messages and says when to stop.
pub fn spawn_with<M, P, R, B>(
    to: &B,
    name: &str,
    work: impl FnOnce(&Report<P>) -> R + Send + 'static,
    progress: impl Fn(P) -> M + Send + 'static,
    done: impl FnOnce(Result<R, Failed>) -> M + Send + 'static,
) -> Job
where
    B: Mailbox<M>,
    P: 'static,
    R: 'static,
    M: 'static,
{
    let cancel: Arc<AtomicBool> = Arc::default();
    let (mailbox, flag) = (to.clone(), Arc::clone(&cancel));
    spawn(
        to,
        name,
        move || {
            let report = Report {
                post: Box::new(move |p| {
                    mailbox.post(progress(p));
                }),
                cancel: flag,
                last: Cell::new(None),
            };
            work(&report)
        },
        done,
    );
    Job {
        cancel,
        detached: false,
    }
}

#[cfg(test)]
mod tests;
