//! `init`'s supervision decisions (issues #93, #549), as pure functions the
//! host can test: what a row's exit leads to (a restart after a backoff, a
//! failure, or a clean stop), and when the desktop must be told that an app
//! the user opened is gone.
//!
//! Services and the desktop shell keep the classic policy: restart on the
//! exits their [`Restart`] policy names, with a doubling backoff, and give up
//! after [`MAX_RESTARTS`] *rapid* crashes (a run of [`STABLE_TICKS`] wipes the
//! count). A launched **app** is different. A start-up failure (a bad script,
//! a missing permission, a missing file) is deterministic, so restarting it
//! only flickers its window open and shut until the budget runs out. An app
//! with the `OnFailure` policy that fails within [`STARTUP_TICKS`] of a start
//! is therefore not restarted, and an app the user killed (a terminating
//! signal) is simply stopped. When an app row ends failed, [`tells_desktop`]
//! says so and `init` publishes `system/events/app/<id>` for the shell's
//! notice, with the app's own [`clean_reason`] when it reported one. A
//! *resident* app (docs/tray-plan.md) is kept running: it is restarted
//! after a crash past start-up whatever its manifest's policy says.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::String;

mod stop;
pub use stop::{
    logout_deadline, logout_settled, quit_deadline, stop_mode, StopMode, LOGOUT_REAP_TICKS,
    QUIT_GRACE_TICKS,
};

/// First restart delay (PIT ticks, 100 Hz), doubled per rapid crash.
pub const BACKOFF_BASE: u64 = 10;
/// Restart delay cap, so a crash loop stays gentle.
pub const BACKOFF_MAX: u64 = 300;
/// A row that stayed up this long is considered recovered: its restart
/// counter resets, so occasional crashes never exhaust the budget.
pub const STABLE_TICKS: u64 = 100;
/// Give up restarting a row after this many *rapid* crashes.
pub const MAX_RESTARTS: u64 = 5;
/// An app that fails this soon after a start failed starting up (ten
/// seconds: a LazyRAD app compiles its scripts first, slowly under
/// emulation), and is not restarted.
pub const STARTUP_TICKS: u64 = 1000;
/// Most bytes of an app's failure reason `init` keeps.
pub const MAX_REASON_BYTES: usize = 512;

/// What to do when a row exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restart {
    /// Restart on any exit.
    Always,
    /// Restart only when the exit status is non-zero.
    OnFailure,
    /// Never restart.
    Once,
}

impl Restart {
    /// The wire word `ListApps` reports.
    pub fn label(self) -> &'static str {
        match self {
            Restart::Always => "always",
            Restart::OnFailure => "on-failure",
            Restart::Once => "once",
        }
    }
}

/// One exit, as the supervisor saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exit {
    /// The row's restart policy.
    pub policy: Restart,
    /// Whether the row is a launched app (vs a boot manifest service).
    pub app: bool,
    /// The exit status (`128 + signal` for a signal).
    pub status: u64,
    /// Ticks the run lasted.
    pub uptime: u64,
    /// The row's rapid-crash count before this exit.
    pub restarts: u64,
    /// Whether the app is resident (docs/tray-plan.md section 5): whatever
    /// its manifest says, a crash after start-up restarts it with backoff,
    /// while a clean exit, a kill by the user and a failure while starting
    /// do not. (An exit after a `Quit` never reaches [`decide`]: `init`
    /// retires the row when it asks the app to quit.)
    pub resident: bool,
}

/// Why a row ended failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// It kept crashing: [`MAX_RESTARTS`] rapid restarts.
    Exhausted,
    /// It exited with an error and its policy restarts nothing.
    NoPolicy,
    /// An app failed while starting up.
    StartUp,
}

impl Cause {
    /// The human-readable detail of the service event.
    pub fn detail(self) -> &'static str {
        match self {
            Cause::Exhausted => "restart budget exhausted",
            Cause::NoPolicy => "exited with an error and has no restart policy",
            Cause::StartUp => "failed while starting; not restarted",
        }
    }
}

/// What an exit leads to. `restarts` is the row's new rapid-crash count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Start it again `delay` ticks from now.
    Restart { restarts: u64, delay: u64 },
    /// Leave it failed.
    Failed { restarts: u64, cause: Cause },
    /// It finished (or was stopped on purpose); nothing more to do.
    Stopped { restarts: u64 },
}

/// Decide what `exit` leads to (see the crate docs for the rules).
pub fn decide(exit: Exit) -> Outcome {
    let exit = if exit.resident && exit.app {
        Exit {
            policy: Restart::OnFailure,
            ..exit
        }
    } else {
        exit
    };
    let mut restarts = if exit.uptime >= STABLE_TICKS {
        0
    } else {
        exit.restarts
    };
    let app_policy = exit.app && exit.policy != Restart::Always;
    if app_policy && killed_by_user(exit.status) {
        return Outcome::Stopped { restarts };
    }
    let failed_starting = app_policy
        && exit.policy == Restart::OnFailure
        && exit.status != 0
        && exit.uptime < STARTUP_TICKS;
    if failed_starting {
        return Outcome::Failed {
            restarts,
            cause: Cause::StartUp,
        };
    }
    if restarts_after(exit.policy, exit.status) {
        restarts += 1;
        if restarts >= MAX_RESTARTS {
            Outcome::Failed {
                restarts,
                cause: Cause::Exhausted,
            }
        } else {
            Outcome::Restart {
                restarts,
                delay: backoff(restarts),
            }
        }
    } else if exit.status != 0 {
        Outcome::Failed {
            restarts,
            cause: Cause::NoPolicy,
        }
    } else {
        Outcome::Stopped { restarts }
    }
}

/// Whether a row with restart policy `policy` comes back after exiting with
/// `status`. `Always` covers every exit, including a kill (the desktop shell
/// relies on it: killing LazyShell brings it back).
pub fn restarts_after(policy: Restart, status: u64) -> bool {
    match policy {
        Restart::Always => true,
        Restart::OnFailure => status != 0,
        Restart::Once => false,
    }
}

/// Capped exponential backoff in PIT ticks for the `restarts`-th restart.
pub fn backoff(restarts: u64) -> u64 {
    let shift = restarts.saturating_sub(1).min(6);
    (BACKOFF_BASE << shift).min(BACKOFF_MAX)
}

/// Whether the desktop is told about this exit: a launched app (never the
/// shell, which restarts forever, nor a service) that ended failed.
pub fn tells_desktop(exit: &Exit, outcome: &Outcome) -> bool {
    exit.app && exit.policy != Restart::Always && matches!(outcome, Outcome::Failed { .. })
}

/// Whether `status` is a terminating signal a person sends (hang-up,
/// interrupt, kill, terminate): stopping an app, not a crash.
pub fn killed_by_user(status: u64) -> bool {
    matches!(status.checked_sub(128), Some(1 | 2 | 9 | 15))
}

/// What a person reads for `status`: `exit code 2`, or `signal 11
/// (segmentation fault)` for a fault.
pub fn describe_status(status: u64) -> String {
    match status
        .checked_sub(128)
        .filter(|signal| (1..=64).contains(signal))
    {
        Some(signal) => match signal_name(signal) {
            Some(name) => format!("signal {signal} ({name})"),
            None => format!("signal {signal}"),
        },
        None => format!("exit code {status}"),
    }
}

/// The common signals' plain names.
fn signal_name(signal: u64) -> Option<&'static str> {
    Some(match signal {
        1 => "hang-up",
        2 => "interrupted",
        4 => "illegal instruction",
        6 => "aborted",
        7 => "bus error",
        8 => "arithmetic error",
        9 => "killed",
        11 => "segmentation fault",
        15 => "terminated",
        _ => return None,
    })
}

/// An app's failure reason as `init` keeps it: control characters (newlines
/// included) become spaces, runs of spaces collapse, the ends are trimmed,
/// and at most [`MAX_REASON_BYTES`] bytes remain, cut on a character
/// boundary. The text is untrusted: it is shown, never parsed.
pub fn clean_reason(text: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for ch in text.chars() {
        let ch = if ch.is_control() { ' ' } else { ch };
        if ch == ' ' {
            space = !out.is_empty();
            continue;
        }
        let extra = usize::from(space) + ch.len_utf8();
        if out.len() + extra > MAX_REASON_BYTES {
            break;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests;
