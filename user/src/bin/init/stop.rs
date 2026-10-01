//! `init`'s `Stop(app)`: end every running instance of an app, for the package
//! manager removing it (`pkgd` calls this before it deletes the app's files and
//! revokes its policy, so nothing keeps running on a policy that is gone).
//!
//! The kill is the native `kill` syscall (28) on the child `init` spawned. The
//! row is retired as `Stopped` *before* the exit is reaped, so the supervisor's
//! restart policy never respawns what was stopped on purpose.

use alloc::format;
use alloc::vec::Vec;

use user::messenger::{self, router};
use user::sys::{self, Cred as SysCred};

use super::service::{Phase, Service};
use super::state::CAP_SETUID;
use super::supervise::publish_state;

/// Stop every running instance of the launched app `app`; returns how many.
///
/// Root and a holder of `CAP_SETUID` (the package manager) may stop any
/// instance; anyone else may stop instances in their own session only, and a
/// request that would touch another session's instance is refused whole
/// (`-EPERM`) rather than done halfway. An app with nothing running is not an
/// error: the answer is 0.
pub(super) fn stop_app(
    services: &mut [Service],
    broker: &mut router::TopicBroker,
    app: &str,
    caller: &SysCred,
) -> messenger::Result<u64> {
    let privileged = caller.uid == 0 || caller.caps & CAP_SETUID != 0;
    let targets: Vec<usize> = services
        .iter()
        .enumerate()
        .filter(|(_, row)| row.launched && row.name == app && is_live(row))
        .map(|(index, _)| index)
        .collect();
    if !privileged
        && targets
            .iter()
            .any(|index| services[*index].cred.map(|cred| cred.session) != Some(caller.session))
    {
        return Err(messenger::Error::Errno(-messenger::errno::EPERM));
    }
    let mut stopped = 0;
    for index in targets {
        let pid = services[index].pid;
        // Retire the row first: the exit that follows must not look like a
        // crash to the restart policy.
        services[index].phase = Phase::Stopped;
        services[index].pid = 0;
        services[index].last_status = None;
        if pid != 0 {
            match sys::kill(pid, sys::SIG_KILL) {
                Ok(()) => {}
                // Already gone: the goal is met.
                Err(-3) => {}
                Err(code) => sys::write_str(&format!(
                    "INIT:STOP:KILL:FAIL app={app} pid={pid} errno={code}\n"
                )),
            }
        }
        publish_state(
            broker,
            &services[index],
            "stopped",
            0,
            0,
            0,
            "stopped on request",
        );
        stopped += 1;
    }
    sys::write_str(&format!("INIT:STOP:PASS app={app} stopped={stopped}\n"));
    Ok(stopped)
}

/// A row that holds, or will reclaim, a task: running, or waiting out a
/// restart backoff.
fn is_live(row: &Service) -> bool {
    matches!(
        row.phase,
        Phase::Running | Phase::Restarting | Phase::Pending
    )
}
