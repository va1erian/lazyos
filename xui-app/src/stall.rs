//! Evidence for UI-thread stalls: a line on the program log for every piece of
//! work that held a UI thread for longer than it should have.
//!
//! A stall is invisible from outside: the task is `blocked` or `runnable`
//! like any other, and `dbgd` has no wait reason. So the places that can
//! block a UI thread time themselves and say so:
//!
//! * every Messenger call ([`crate::sys::msg_call`]: interface id, method,
//!   deadline, outcome),
//! * `Service::connect` (it retries for up to 10 s),
//! * each pass of the backend loop and its input, update and present parts
//!   (`backend/event_loop.rs`), and
//! * each chore of the shell's heartbeat (`shell/desktop.rs`).
//!
//! The line is `UI:STALL kind=<kind> ms=<n> at=<tick> app=<name>
//! thread=<name> what=<detail>`; `at` is the PIT tick the work ended at
//! (compare it with `sysinfo`'s `ticks`, 100 Hz). Read it with
//! `dbgctl log --source programs`. At most [`BUDGET`] lines are printed per
//! [`WINDOW_TICKS`] per thread, then a `UI:STALL:SUPPRESSED` count follows, so
//! a wedged peer cannot flood the ring the evidence lives in. The line goes
//! through [`crate::trace`] (channel `ui`), whole, to the console device: an
//! app started in the Terminal reaches `dbgctl` too, not its pty.

use std::sync::OnceLock;

use crate::sys;
use crate::trace::{self, Budget, Channel};

/// Work shorter than this is not a stall (a frame is 16 ms; this is six).
pub const SLOW_NS: u64 = 100_000_000;
/// A whole loop pass or a present may take longer than a single call.
pub const SLOW_PASS_NS: u64 = 150_000_000;
/// Lines one thread may print per [`WINDOW_TICKS`].
const BUDGET: u32 = 40;
/// The rate window, in PIT ticks (10 s).
const WINDOW_TICKS: u64 = 1000;

thread_local! {
    /// This thread's budget (the channel itself carries `UI:RECT` evidence
    /// that must never be dropped, so only the stall is limited).
    static RATE: Budget = const { Budget::new(BUDGET, WINDOW_TICKS) };
}

/// A stopwatch start: the monotonic clock now (nothing in a `LAZYOS_TRACE=0`
/// build, whose [`finish`] never prints).
pub fn start() -> u64 {
    if trace::ENABLED {
        sys::monotonic_ns()
    } else {
        0
    }
}

/// This program's name, once.
fn app() -> &'static str {
    static APP: OnceLock<String> = OnceLock::new();
    APP.get_or_init(|| {
        std::env::args()
            .next()
            .and_then(|path| path.rsplit('/').next().map(str::to_owned))
            .unwrap_or_else(|| String::from("?"))
    })
}

/// Print a `UI:STALL` line when the work that began at `since` ran past
/// `limit_ns`. `what` is built only then.
pub fn finish(since: u64, limit_ns: u64, kind: &str, what: impl FnOnce() -> String) {
    if !trace::ENABLED {
        return;
    }
    let took = sys::monotonic_ns().saturating_sub(since);
    if took < limit_ns {
        return;
    }
    let allowed = RATE.with(|budget| {
        trace::within_with(budget, Channel::Ui, |count| {
            trace::line!(Channel::Ui, "STALL:SUPPRESSED n={count} app={}", app());
        })
    });
    if !allowed {
        return;
    }
    let thread = std::thread::current();
    trace::line!(
        Channel::Ui,
        "STALL kind={kind} ms={} at={} app={} thread={} what={}",
        took / 1_000_000,
        sys::clock_ticks(),
        app(),
        thread.name().unwrap_or("?"),
        what()
    );
}

/// Run `work`, reporting it as a `chore` stall when it ran past [`SLOW_NS`].
pub fn chore<T>(name: &str, work: impl FnOnce() -> T) -> T {
    let began = start();
    let value = work();
    finish(began, SLOW_NS, "chore", || name.to_owned());
    value
}
