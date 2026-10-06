//! `init`'s `Stop(app)`: end every running instance of an app, for the tray's
//! Quit row, a logout, or the package manager removing it (`pkgd` calls this
//! before it deletes the app's files and revokes its policy, so nothing keeps
//! running on a policy that is gone).
//!
//! An app that watches its lifecycle (or a resident app, which may still
//! come to watch) is asked to quit and given a hard 3 s grace counted from
//! the `Stop` ([`super::lifecycle`]); any other instance is killed at once
//! with the native `kill` syscall (28). Either way the row is retired before
//! its exit is reaped, so the restart policy never respawns what was stopped
//! on purpose, and the `Stop` is answered only once every target has exited
//! (docs/tray-plan.md section 5): the caller learns of the reap, not of a
//! signal still in flight.

use alloc::format;
use alloc::vec::Vec;

use user::messenger::{self, router};
use user::sys::{self, Cred as SysCred};

use super::lifecycle;
use super::service::{Phase, Service};
use super::state::CAP_SETUID;
use super::supervise::publish_state;

/// What a `Stop` did: how many instances it stopped, and the tasks whose
/// exits it must wait for before answering.
pub(super) struct Stopped {
    pub(super) count: u64,
    pub(super) pids: Vec<u64>,
}

/// Stop every running instance of the launched app `app`.
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
) -> messenger::Result<Stopped> {
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
    let now = sys::clock();
    let mut stopped = Stopped {
        count: 0,
        pids: Vec::new(),
    };
    for index in targets {
        let row = &mut services[index];
        let pid = row.pid;
        stopped.count += 1;
        if pid != 0 {
            stopped.pids.push(pid);
        }
        if row.phase == Phase::Stopping {
            // Already quitting from an earlier `Stop`: just wait for it too.
            continue;
        }
        if pid != 0 && lifecycle::graceful(row) {
            lifecycle::begin_quit(row, now);
            publish_state(broker, row, "stopping", pid, 0, 0, "quit on request");
            continue;
        }
        // Retire the row first: the exit that follows must not look like a
        // crash to the restart policy.
        row.phase = Phase::Stopped;
        row.pid = 0;
        row.last_status = None;
        if pid != 0 {
            match sys::kill(pid, sys::SIG_KILL) {
                Ok(()) => {}
                // Already gone: the goal is met, and there is no exit to wait for.
                Err(-3) => stopped.pids.retain(|&target| target != pid),
                Err(code) => sys::write_str(&format!(
                    "INIT:STOP:KILL:FAIL app={app} pid={pid} errno={code}\n"
                )),
            }
        }
        publish_state(broker, row, "stopped", 0, 0, 0, "stopped on request");
    }
    sys::write_str(&format!(
        "INIT:STOP:PASS app={app} stopped={}\n",
        stopped.count
    ));
    Ok(stopped)
}

/// A row that holds, or will reclaim, a task: running, quitting, or waiting
/// out a restart backoff. A `Stopping` row of a shutdown is not the app's to
/// stop again.
fn is_live(row: &Service) -> bool {
    match row.phase {
        Phase::Running | Phase::Restarting | Phase::Pending => true,
        Phase::Stopping => row.life.quit,
        Phase::Stopped | Phase::Failed => false,
    }
}
