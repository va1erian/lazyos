//! Calls made on the process's first thread, which is the window's: the shell's notification service
//! names an app by the task of the sender, and a message from any other thread (the toasts' worker,
//! the downloads' notifier) carries that thread's id and is refused as "not an app".
//!
//! A job is queued and the window is woken ([`install`]); the window runs every queued job when the
//! wake reaches it ([`run`]), between two of its own messages.

use std::collections::VecDeque;

use super::*;

/// How long a call into the main thread is waited for: the window runs it at its next turn.
const MAIN_WAIT: Duration = Duration::from_secs(3);
/// A wait longer than this is worth a line (the window was busy), as is one that timed out.
const MAIN_SLOW: Duration = Duration::from_millis(1000);
/// The most `STALL:on_main` lines a minute prints.
const MAIN_STALL_LINES: u32 = 6;

type Job = Box<dyn FnOnce() + Send>;
type Wake = Box<dyn Fn() + Send + Sync>;

struct Queue {
    jobs: VecDeque<Job>,
    wake: Option<Wake>,
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue {
    jobs: VecDeque::new(),
    wake: None,
});

fn queue() -> std::sync::MutexGuard<'static, Queue> {
    QUEUE.lock().unwrap_or_else(|e| e.into_inner())
}

/// How the window is woken for its jobs (its proxy); jobs queued before are woken for now.
pub(super) fn install(wake: impl Fn() + Send + Sync + 'static) {
    let waiting = {
        let mut queue = queue();
        queue.wake = Some(Box::new(wake));
        !queue.jobs.is_empty()
    };
    if waiting {
        wake_window();
    }
}

fn wake_window() {
    if let Some(wake) = queue().wake.as_ref() {
        wake();
    }
}

/// The window's turn: runs every job queued (and the ones those queue).
pub(super) fn run() {
    loop {
        let job = queue().jobs.pop_front();
        match job {
            Some(job) => job(),
            None => break,
        }
    }
}

/// Runs `job` on the main thread and returns at once; nobody waits for it.
pub(super) fn on_main_detached(job: impl FnOnce() + Send + 'static) {
    queue().jobs.push_back(Box::new(job));
    wake_window();
}

/// Runs `job` on the main thread and returns what it returned; `None` when the window did not get to
/// it in [`MAIN_WAIT`] (the job then still runs, its answer unread). Only for a worker's own thread:
/// the window's thread never calls this (it would wait for itself). `what` names the call in the
/// `STALL:on_main` line a slow or timed-out wait prints.
pub(super) fn on_main<T: Send + 'static>(
    what: &'static str,
    job: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (answer, reply) = mpsc::channel();
    let began = Instant::now();
    on_main_detached(move || {
        let _ = answer.send(job());
    });
    let result = reply.recv_timeout(MAIN_WAIT).ok();
    let waited = began.elapsed();
    if result.is_none() || waited >= MAIN_SLOW {
        static BUDGET: xui_app::trace::Budget = xui_app::trace::Budget::new(MAIN_STALL_LINES, 6000);
        if xui_app::trace::within(&BUDGET, xui_app::trace::Channel::Webplus) {
            crate::marker(&format!(
                "STALL:on_main:{what}:{}{}",
                waited.as_millis(),
                if result.is_none() { ":timeout" } else { "" }
            ));
        }
    }
    result
}
