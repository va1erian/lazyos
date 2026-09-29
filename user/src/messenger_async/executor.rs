use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::task::Wake;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll, Waker};

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

/// Run one future to completion on the current task.
///
/// This is the whole executor: poll, and on `Pending` poll again. The leaf
/// futures in this module never return `Pending` (they park the task in the
/// kernel), so the loop makes progress without a scheduler; a composed future
/// that yields is re-polled immediately, which is the documented trade-off
/// described in the module docs.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let flag = FlagWaker::new();
    let waker = Waker::from(flag);
    let mut context = Context::from_waker(&waker);
    let mut future = core::pin::pin!(future);
    loop {
        match Future::poll(future.as_mut(), &mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => core::hint::spin_loop(),
        }
    }
}

/// A single-task executor: spawn `Future<Output = ()>` jobs and poll them
/// round-robin until every job finishes.
///
/// Jobs are expected to block in the kernel when they have nothing to do; see
/// [`block_on`] for the motivation. [`Executor::poll_ready`] is a single pass,
/// [`Executor::run`] drains the queue.
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
        let _ = flag.take();
        completed
    }

    /// Poll the queue until every job has completed.
    pub fn run(&mut self) {
        while !self.tasks.is_empty() {
            self.poll_ready();
        }
    }
}
