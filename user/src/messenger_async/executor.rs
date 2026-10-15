use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::task::Wake;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll, Waker};

use crate::messenger::errno;
use crate::messenger::wait::WAIT_CALLS;

// ---------------------------------------------------------------------------
// Minimal executor and waker
// ---------------------------------------------------------------------------

/// Waker state: a single "someone asked to be rescheduled" flag.
pub(super) struct FlagWaker {
    woken: AtomicBool,
}

impl FlagWaker {
    pub(super) fn new() -> Arc<FlagWaker> {
        Arc::new(FlagWaker {
            woken: AtomicBool::new(false),
        })
    }

    /// Whether a `wake` landed since the flag was last cleared.
    fn take(&self) -> bool {
        self.woken.swap(false, Ordering::AcqRel)
    }
}

impl Wake for FlagWaker {
    fn wake(self: Arc<Self>) {
        self.woken.store(true, Ordering::Release);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.woken.store(true, Ordering::Release);
    }
}

/// How long [`block_on`] and [`Executor::run`] park when nothing can notice
/// their work (no tracked call, and the wait of one was refused): they look
/// again after this rather than spinning the CPU (P7.4).
const IDLE_PARK_NS: u64 = 1_000_000;

/// Park until something can wake a pending leaf future: the tracked call
/// transactions ([`super::future`]'s registry, whose completions ring the
/// kernel's `MESSENGER` queue: issue #725). `false` when nothing is tracked,
/// or the wait was refused for a reason a rescan cannot repair, so the
/// caller parks the old one-tick way. A vanished transaction is repaired:
/// the pruned future was dropped (or its task died), and its id would
/// otherwise turn every later wait into a failed one.
fn park_pending() -> bool {
    use crate::messenger::wait::{wait_items, WaitItem};
    let items = super::future::pending::items();
    if items.is_empty() {
        return false;
    }
    match wait_items(&items, WAIT_CALLS, None) {
        // Nothing is taken; one pass of `pass` reads what became ready.
        Ok(_) => true,
        Err(error) if error.errno() == Some(-errno::ENOENT) => {
            let ids = super::future::pending::ids();
            for txn in ids {
                let probe = [WaitItem::Call(txn)];
                if let Err(error) = wait_items(&probe, 0, Some(0)) {
                    // A pending transaction answers only `-ETIMEDOUT` on a
                    // zero deadline; the vanished ones (their task died)
                    // are the only entries worth untracking.
                    if error.errno() == Some(-errno::ENOENT) {
                        super::future::pending::untrack(txn);
                    }
                }
            }
            true
        }
        Err(_) => false,
    }
}

/// Run one future to completion on the current task.
///
/// This is the whole executor: poll, and on `Pending` wait for what it can
/// join with. A leaf [`Call`](super::future::Call) parks nothing while it
/// polls (`OP_CALL_CHECK`), so this loop parks on the *kernel* wake the
/// completion queue provides instead of a timer, and re-polls the moment a
/// transaction ends. A composed future that wakes its waker is re-polled at
/// once; one that returns `Pending` without a wake and without a tracked
/// call is waiting on something no waker reports, so the task parks
/// [`IDLE_PARK_NS`] before looking again instead of spinning.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let flag = FlagWaker::new();
    let waker = Waker::from(flag.clone());
    let mut context = Context::from_waker(&waker);
    let mut future = core::pin::pin!(future);
    loop {
        match Future::poll(future.as_mut(), &mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending if flag.take() => {}
            Poll::Pending => {
                if !park_pending() {
                    crate::sys::sleep_ns(IDLE_PARK_NS);
                }
                flag.take();
            }
        }
    }
}

/// A single-task executor: spawn `Future<Output = ()>` jobs and poll them
/// round-robin until every job finishes.
///
/// Jobs are expected to block in the kernel when they have nothing to do;
/// see [`block_on`] for the motivation. [`Executor::poll_ready`] is a single
/// pass, [`Executor::run`] drains the queue.
#[derive(Default)]
pub struct Executor {
    tasks: Vec<Pin<Box<dyn Future<Output = ()>>>>,
}

impl Executor {
    /// An executor with no jobs.
    pub fn new() -> Executor {
        Executor { tasks: Vec::new() }
    }

    /// Queue a job. `run` polls it until it resolves.
    pub fn spawn<F>(&mut self, future: F)
    where
        F: Future<Output = ()> + 'static,
    {
        self.tasks.push(Box::pin(future));
    }

    /// Number of unfinished jobs.
    pub fn pending(&self) -> usize {
        self.tasks.len()
    }

    /// Whether no jobs are queued.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Poll every job once, removing the ones that completed. Returns how many
    /// jobs resolved during this pass.
    pub fn poll_ready(&mut self) -> usize {
        self.pass().0
    }

    /// One pass: how many jobs resolved, and whether any asked to be polled
    /// again (woke its waker).
    fn pass(&mut self) -> (usize, bool) {
        let flag = FlagWaker::new();
        let waker = Waker::from(flag.clone());
        let mut context = Context::from_waker(&waker);
        let mut completed = 0;
        let mut index = 0;
        while index < self.tasks.len() {
            match self.tasks[index].as_mut().poll(&mut context) {
                Poll::Ready(()) => {
                    drop(self.tasks.swap_remove(index));
                    completed += 1;
                }
                Poll::Pending => index += 1,
            }
        }
        (completed, flag.take())
    }

    /// Poll the queue until every job has completed. A pass that finished
    /// nothing and woke nothing parks on its pending call completions (the
    /// kernel's `WAIT_CALLS` wake, issue #725), or for a tick when nothing
    /// can report one (see [`block_on`]).
    pub fn run(&mut self) {
        while !self.tasks.is_empty() {
            let (completed, woken) = self.pass();
            if completed == 0 && !woken && !self.tasks.is_empty() && !park_pending() {
                crate::sys::sleep_ns(IDLE_PARK_NS);
            }
        }
    }
}
