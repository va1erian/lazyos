//! Calls into WebKit's main thread, which is the process's first thread (the shell's notification
//! service names an app by its task and refuses any other).

use super::*;

extern "C" {
    /// `lazyos_ui.h`: runs `task(context)` once on WebKit's main thread and returns.
    fn lzw_engine_run_on_main(task: extern "C" fn(*mut c_void), context: *mut c_void);
}

/// How long a call into the main thread is waited for. The main thread is WebKit's run loop, which
/// only ever waits for the shell's answer (bounded) inside what is sent to it.
const MAIN_WAIT: Duration = Duration::from_secs(3);
/// A wait longer than this is worth a line (the main thread was busy), as is one that timed out.
const MAIN_SLOW: Duration = Duration::from_millis(1000);
/// The most `STALL:on_main` lines a run prints.
const MAIN_STALL_LINES: u32 = 20;

/// Runs `job` on WebKit's main thread and returns at once; nobody waits for it.
///
/// The shell's notification service names an app by the task id of the sender, and a message from
/// a thread other than the process's first carries the thread's id: the window's thread, and a
/// worker's, are refused as "not an app". The first thread is WebKit's run loop, so the calls the
/// window makes of that service are made there.
pub(super) fn on_main_detached(job: impl FnOnce() + Send + 'static) {
    extern "C" fn run(context: *mut c_void) {
        // SAFETY: `context` is the pointer `Box::into_raw` made in `on_main_detached`, handed over once.
        let job = unsafe { Box::from_raw(context.cast::<Box<dyn FnOnce() + Send>>()) };
        job();
    }
    let job: Box<dyn FnOnce() + Send> = Box::new(job);
    let context = Box::into_raw(Box::new(job)).cast::<c_void>();
    // SAFETY: `run` takes the pointer back exactly once; the engine calls it once, on its thread.
    unsafe { lzw_engine_run_on_main(run, context) };
}

/// Runs `job` on WebKit's main thread and returns what it returned; `None` when the thread did not
/// get to it in [`MAIN_WAIT`] (the job then still runs, its answer unread). Only for a worker's own
/// thread, which has nothing else to do while it waits: the window's thread never calls this, and a
/// call whose answer nobody needs uses [`on_main_detached`]. `what` names the call in the
/// `STALL:on_main` line a slow or timed-out wait prints.
pub(super) fn on_main<T: Send + 'static>(what: &'static str, job: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (answer, reply) = mpsc::channel();
    let began = Instant::now();
    on_main_detached(move || {
        let _ = answer.send(job());
    });
    let result = reply.recv_timeout(MAIN_WAIT).ok();
    let waited = began.elapsed();
    if result.is_none() || waited >= MAIN_SLOW {
        static LINES: AtomicU32 = AtomicU32::new(0);
        if LINES.fetch_add(1, Ordering::Relaxed) < MAIN_STALL_LINES {
            crate::marker(&format!(
                "STALL:on_main:{what}:{}{}",
                waited.as_millis(),
                if result.is_none() { ":timeout" } else { "" }
            ));
        }
    }
    result
}

