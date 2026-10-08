//! One stop rule for launched apps (issue #651, docs/tray-plan.md section 5):
//! `init.Stop`, a logout and the shutdown's apps stage all stop a launched
//! row the same way, with the same fixed grace.
//!
//! * A row that watches its lifecycle channel, or a resident app that may
//!   still come to watch, is asked to [`StopMode::Quit`]: `Quit(grace)` on
//!   `os.lazy.init.app.v1`, so it can save.
//! * Any other running row gets [`StopMode::Terminate`]: `SIGTERM`, whose
//!   default action ends it at once, while a program that handles it (a C
//!   program, a shell) still gets its chance to clean up.
//! * A row that holds no task (waiting out a restart backoff, or not started
//!   yet) is simply retired: [`StopMode::Retire`].
//!
//! Either way the row is killed when [`QUIT_GRACE_TICKS`] (3 s) have passed
//! since the stop request: one number, never extended, counted from the
//! request rather than from the app's reaction.

/// The grace a stopped app gets before it is killed: 3 s at 100 Hz, for
/// every app and every path (no per-package override).
pub const QUIT_GRACE_TICKS: u64 = 300;

/// How long a logout waits past the grace for the kills to be reaped before
/// it sweeps the session's remaining tasks anyway (0.5 s).
pub const LOGOUT_REAP_TICKS: u64 = 50;

/// How a launched row is asked to stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopMode {
    /// No task to stop: retire the row.
    Retire,
    /// `Quit(grace)` on its lifecycle channel (now, or at its `Watch`).
    Quit,
    /// `SIGTERM` now.
    Terminate,
}

/// The one rule: how to stop a launched row that `has_task`, `watching` its
/// lifecycle channel or not, `resident` or not.
pub fn stop_mode(has_task: bool, watching: bool, resident: bool) -> StopMode {
    if !has_task {
        StopMode::Retire
    } else if watching || resident {
        StopMode::Quit
    } else {
        StopMode::Terminate
    }
}

/// The tick a stop requested at `now` kills what is left.
pub fn quit_deadline(now: u64) -> u64 {
    now.saturating_add(QUIT_GRACE_TICKS)
}

/// The latest tick a logout started at `started` waits until before it
/// sweeps the session's other tasks: the grace, plus time to reap its kills.
pub fn logout_deadline(started: u64) -> u64 {
    quit_deadline(started).saturating_add(LOGOUT_REAP_TICKS)
}

/// Whether a logout started at `started` is done waiting: none of the
/// session's rows is still `quitting`, or [`logout_deadline`] has passed.
/// Bounded by construction: the login screen never waits past the deadline.
pub fn logout_settled(quitting: usize, started: u64, now: u64) -> bool {
    quitting == 0 || now >= logout_deadline(started)
}
