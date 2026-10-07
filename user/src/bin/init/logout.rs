//! Logout (issue #623): when `logind` announces that a session ended
//! (`system/events/login/end`, believed only from `logind`'s own task, see
//! `sessions.rs`), `init` ends **every task of that session**.
//!
//! Two passes, because the session's tasks are not all `init`'s children:
//!
//! 1. every launched row stamped with the session (LazyShell, the apps, the
//!    Terminal) is retired as `Stopped` *before* its task is killed, so the
//!    restart policy never brings it back;
//! 2. every other task the kernel stamped with the session id (what those
//!    programs spawned: the Terminal's shell and its commands, a script's
//!    children) is found by reading each task slot's credentials (`init`
//!    holds `CAP_SETUID`, which that read needs) and killed. A child inherits
//!    its parent's session, so this reaches everything the session started;
//!    the sweep repeats a few times so a task forked during it is caught.
//!
//! Serial: `INIT:LOGOUT:PASS session=<id> rows=<n> tasks=<n>`, or
//! `INIT:LOGOUT:LEFT session=<id> tasks=<n>` when tasks survived every sweep.

use alloc::format;
use alloc::vec::Vec;

use user::messenger::router;
use user::sys::{self, Cred as SysCred};
use user::sysinfo::MAX_TASKS;

use super::service::{Phase, Service};
use super::supervise::publish_state;

/// Sweeps of the task table: a task forked between two reads is caught by
/// the next one.
const SWEEPS: usize = 4;

/// End every task of `session`.
pub(super) fn end_session(
    services: &mut [Service],
    broker: &mut router::TopicBroker,
    session: u64,
) {
    if session == 0 {
        // Session 0 is the system's own: never a login's to end.
        return;
    }
    let rows = retire_rows(services, broker, session);
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
            "INIT:LOGOUT:PASS session={session} rows={rows} tasks={}\n",
            killed.len()
        ));
    } else {
        sys::write_str(&format!(
            "INIT:LOGOUT:LEFT session={session} tasks={}\n",
            left.len()
        ));
    }
}

/// Retire and kill the launched rows of `session`; returns how many.
fn retire_rows(services: &mut [Service], broker: &mut router::TopicBroker, session: u64) -> usize {
    let mut retired = 0;
    for row in services.iter_mut() {
        let live = matches!(
            row.phase,
            Phase::Running | Phase::Restarting | Phase::Pending | Phase::Stopping
        );
        if !(row.launched && live && row.cred.map(|cred| cred.session) == Some(session)) {
            continue;
        }
        let pid = row.pid;
        // Retire first: the exit that follows is not a crash to restart.
        row.phase = Phase::Stopped;
        row.pid = 0;
        row.last_status = None;
        if pid != 0 {
            let _ = sys::kill(pid, sys::SIG_KILL);
        }
        publish_state(broker, row, "stopped", 0, 0, 0, "session ended");
        retired += 1;
    }
    retired
}

/// The task slots whose kernel-stamped session is `session` (never `init`'s
/// own: it is in session 0, which [`end_session`] refuses).
fn session_tasks(session: u64) -> Vec<u64> {
    (1..MAX_TASKS as u64)
        .filter(|slot| {
            let mut cred = SysCred::default();
            sys::cred_get(Some(*slot), &mut cred).is_ok() && cred.session == session
        })
        .collect()
}
