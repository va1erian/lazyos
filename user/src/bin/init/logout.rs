//! Logout (issues #623, #651): when `logind` announces that a session ended
//! (`system/events/login/end`, believed only from `logind`'s own task, see
//! `sessions.rs`), `init` ends **every task of that session**.
//!
//! Two passes, because the session's tasks are not all `init`'s children:
//!
//! 1. every launched row stamped with the session (LazyShell, the apps, the
//!    Terminal) is stopped by the one rule `Stop` and the shutdown share
//!    ([`lifecycle::begin_stop`]): `Quit` for an app that watches its
//!    lifecycle or is resident, so it can save, `SIGTERM` for any other, and
//!    a kill when the fixed 3 s grace counted from the logout runs out. A
//!    stopped row is never restarted;
//! 2. once every row has exited, or the grace (plus a short reap allowance,
//!    `svcpolicy::logout_deadline`) has passed, every other task the kernel
//!    stamped with the session id (what those programs spawned: the
//!    Terminal's shell and its commands, a script's children) is found by
//!    reading each task slot's credentials (`init` holds `CAP_SETUID`, which
//!    that read needs) and killed. A child inherits its parent's session, so
//!    this reaches everything the session started; the sweep repeats a few
//!    times so a task forked during it is caught.
//!
//! `init` is one task, so the wait between the passes is timer-driven, like
//! `Stop`'s deferred reply: [`Logouts`] holds the ending sessions and the
//! supervision loop steps them. While one is pending, the login screen is
//! not started ([`Logouts::holds_greeter`]; `logind` retries), so it comes
//! back once the session's apps are gone, and never later than the deadline.
//!
//! Serial: `INIT:LOGOUT:BEGIN session=<id>`, the rows' `INIT:APP:QUIT:SENT`
//! or `INIT:APP:TERM:SENT`, `INIT:LOGOUT:WAIT session=<id> rows=<n>
//! quitting=<n>`, `INIT:LOGOUT:GREETER:HELD` per refused login screen, then
//! `INIT:LOGOUT:PASS session=<id> rows=<n> tasks=<n> ticks=<n>`, or
//! `INIT:LOGOUT:LEFT session=<id> tasks=<n>` when tasks survived every sweep.

use alloc::format;
use alloc::vec::Vec;

use svcpolicy::{logout_deadline, logout_settled, StopMode};
use user::messenger::router;
use user::sys;
use user::sysinfo::MAX_TASKS;

use super::lifecycle;
use super::service::{Phase, Service};
use super::supervise::publish_state;

/// Sweeps of the task table: a task forked between two reads is caught by
/// the next one.
const SWEEPS: usize = 4;

/// One session whose rows were asked to stop, waiting for them to exit.
struct Ending {
    session: u64,
    started: u64,
    rows: usize,
}

/// The sessions being logged out.
#[derive(Default)]
pub(super) struct Logouts {
    ending: Vec<Ending>,
}

impl Logouts {
    /// `session` ended: stop its launched rows (pass 1) and wait for them.
    pub(super) fn begin(
        &mut self,
        services: &mut [Service],
        broker: &mut router::TopicBroker,
        session: u64,
    ) {
        // Session 0 is the system's own: never a login's to end.
        if session == 0 || self.ending.iter().any(|ending| ending.session == session) {
            return;
        }
        let started = sys::clock();
        sys::write_str(&format!("INIT:LOGOUT:BEGIN session={session}\n"));
        let rows = stop_rows(services, broker, session, started);
        sys::write_str(&format!(
            "INIT:LOGOUT:WAIT session={session} rows={rows} quitting={}\n",
            quitting(services, session)
        ));
        self.ending.push(Ending {
            session,
            started,
            rows,
        });
        // Nothing to wait for: sweep at once.
        self.step(services, started);
    }

    /// Finish every logout whose rows are gone or whose deadline passed.
    pub(super) fn step(&mut self, services: &[Service], now: u64) {
        let (done, waiting): (Vec<Ending>, Vec<Ending>) = core::mem::take(&mut self.ending)
            .into_iter()
            .partition(|ending| {
                logout_settled(quitting(services, ending.session), ending.started, now)
            });
        self.ending = waiting;
        for ending in done {
            sweep_session(&ending, now);
        }
    }

    /// When the loop must look again with no exit to wake it.
    pub(super) fn next_deadline(&self) -> Option<u64> {
        self.ending
            .iter()
            .map(|ending| logout_deadline(ending.started))
            .min()
    }

    /// Whether the login screen must wait: a logout is still stopping apps.
    pub(super) fn holds_greeter(&self) -> bool {
        !self.ending.is_empty()
    }
}

/// Pass 1: stop the launched rows of `session`; returns how many.
fn stop_rows(
    services: &mut [Service],
    broker: &mut router::TopicBroker,
    session: u64,
    now: u64,
) -> usize {
    let mut stopped = 0;
    for row in services.iter_mut() {
        let live = matches!(
            row.phase,
            Phase::Running | Phase::Restarting | Phase::Pending
        );
        if !(row.launched && live && row.cred.map(|cred| cred.session) == Some(session)) {
            continue;
        }
        let pid = row.pid;
        match lifecycle::begin_stop(row, now) {
            StopMode::Retire => publish_state(broker, row, "stopped", 0, 0, 0, "session ended"),
            StopMode::Quit | StopMode::Terminate => {
                publish_state(broker, row, "stopping", pid, 0, 0, "session ended")
            }
        }
        stopped += 1;
    }
    stopped
}

/// The rows of `session` still waiting out their grace (a `Stop` before the
/// logout counts too: its row is the session's).
fn quitting(services: &[Service], session: u64) -> usize {
    services
        .iter()
        .filter(|row| {
            row.launched
                && lifecycle::quitting(row)
                && row.cred.map(|cred| cred.session) == Some(session)
        })
        .count()
}

/// Pass 2: kill every task still stamped with the ending session.
fn sweep_session(ending: &Ending, now: u64) {
    let session = ending.session;
    // A killed task keeps its slot (and its credentials) until its parent
    // reaps it, so a slot already sent SIGKILL, which cannot be caught, is
    // done; only a task the kill could not reach is left.
    let mut killed: Vec<u64> = Vec::new();
    let mut left: Vec<u64> = Vec::new();
    for _ in 0..SWEEPS {
        let fresh: Vec<u64> = session_tasks(session)
            .into_iter()
            .filter(|pid| !killed.contains(pid))
            .collect();
        if fresh.is_empty() {
            left.clear();
            break;
        }
        left.clear();
        for pid in fresh {
            match sys::kill(pid, sys::SIG_KILL) {
                Ok(()) => killed.push(pid),
                // Gone between the read and the kill: the goal is met.
                Err(-3) => {}
                Err(_) => left.push(pid),
            }
        }
    }
    if left.is_empty() {
        sys::write_str(&format!(
            "INIT:LOGOUT:PASS session={session} rows={} tasks={} ticks={}\n",
            ending.rows,
            killed.len(),
            now.saturating_sub(ending.started)
        ));
    } else {
        sys::write_str(&format!(
            "INIT:LOGOUT:LEFT session={session} tasks={}\n",
            left.len()
        ));
    }
}

/// The task slots whose kernel-stamped session is `session` (never `init`'s
/// own: it is in session 0, which [`Logouts::begin`] refuses).
fn session_tasks(session: u64) -> Vec<u64> {
    (1..MAX_TASKS as u64)
        .filter(|slot| sys::cred_get(Some(*slot)).is_ok_and(|cred| cred.session == session))
        .collect()
}
