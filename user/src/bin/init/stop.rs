//! `init`'s `Stop(app)`: end every running instance of an app, for the tray's
//! Quit row or the package manager removing it (`pkgd` calls this before it
//! deletes the app's files and revokes its policy, so nothing keeps running
//! on a policy that is gone).
//!
//! Each instance is stopped by the one rule a logout and the shutdown share
//! ([`lifecycle::begin_stop`], issue #651): `Quit` for an app that watches
//! its lifecycle (or a resident app, which may still come to watch),
//! `SIGTERM` for any other, and a kill once a hard 3 s grace counted from the
//! `Stop` has passed. The row is marked stopping before its exit is reaped,
//! so the restart policy never respawns what was stopped on purpose, and the
//! `Stop` is answered only once every target has exited (docs/tray-plan.md
//! section 5): the caller learns of the reap, not of a signal still in
//! flight.

use alloc::format;
use alloc::vec::Vec;

use svcpolicy::StopMode;
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
/// A holder of `CAP_SETUID` (the package manager, `logind`) may stop any
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
    let privileged = caller.caps & CAP_SETUID != 0;
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
        match lifecycle::begin_stop(row, now) {
            StopMode::Retire => {
                publish_state(broker, row, "stopped", 0, 0, 0, "stopped on request")
            }
            StopMode::Quit | StopMode::Terminate => {
                publish_state(broker, row, "stopping", pid, 0, 0, "quit on request")
            }
        }
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
