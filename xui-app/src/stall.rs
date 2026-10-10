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
//! a wedged peer cannot flood the ring the evidence lives in.

use std::cell::Cell;
use std::sync::OnceLock;

use crate::sys;

/// Work shorter than this is not a stall (a frame is 16 ms; this is six).
pub const SLOW_NS: u64 = 100_000_000;
/// A whole loop pass or a present may take longer than a single call.
pub const SLOW_PASS_NS: u64 = 150_000_000;
/// Lines one thread may print per [`WINDOW_TICKS`].
const BUDGET: u32 = 40;
/// The rate window, in PIT ticks (10 s).
const WINDOW_TICKS: u64 = 1000;

thread_local! {
    /// `(window start tick, lines printed in it, lines suppressed in it)`.
    static RATE: Cell<(u64, u32, u32)> = const { Cell::new((0, 0, 0)) };
}

/// A stopwatch start: the monotonic clock now.
pub fn start() -> u64 {
    sys::monotonic_ns()
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
    let took = sys::monotonic_ns().saturating_sub(since);
    if took < limit_ns {
        return;
    }
    let now = sys::clock_ticks();
    let allowed = RATE.with(|rate| {
        let (begin, printed, suppressed) = rate.get();
        if now.saturating_sub(begin) >= WINDOW_TICKS {
            if suppressed > 0 {
                println!("UI:STALL:SUPPRESSED n={suppressed} app={}", app());
            }
            rate.set((now, 1, 0));
            true
        } else if printed < BUDGET {
            rate.set((begin, printed + 1, suppressed));
            true
        } else {
            rate.set((begin, printed, suppressed + 1));
            false
        }
    });
    if !allowed {
        return;
    }
    let thread = std::thread::current();
    println!(
        "UI:STALL kind={kind} ms={} at={now} app={} thread={} what={}",
        took / 1_000_000,
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
