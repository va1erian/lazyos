//! `init`'s supervision loop helpers: starting ready services, spawning and
//! reaping children, restart backoff, and publishing state events.
//!
//! Split out of `init.rs` (issue #194). An exit during a shutdown retires the
//! row instead of applying its restart policy (docs/shutdown.md).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use user::messenger::{router, services};
use user::sys;

use super::state::{
    Phase, Restart, Service, BACKOFF_BASE, BACKOFF_MAX, MAX_RESTARTS, POLL_TICKS, STABLE_TICKS,
};

/// Start every `Pending` service whose dependencies are `Running`, repeating
/// until no more can start (a single pass suffices for an ordered manifest,
/// but this is order-independent). Launched app rows are spawned by `launch`,
/// never here (they have no dependencies).
pub(super) fn start_ready(services: &mut [Service], broker: &mut router::TopicBroker) {
    loop {
        let mut started = false;
        for index in 0..services.len() {
            if services[index].phase != Phase::Pending {
                continue;
            }
            let deps = services[index].deps;
            let ready = deps.iter().all(|dep| {
                services
                    .iter()
                    .any(|service| service.name == *dep && service.phase == Phase::Running)
            });
            if ready {
                spawn_service(services, index, broker);
                started = true;
            }
        }
        if !started {
            return;
        }
    }
}

/// The credentials a manifest service runs with. Only `inputd` may hold the
/// raw input bus (`CAP_INPUT_RAW`): it gets exactly that capability, and every
/// other service inherits this supervisor's identity minus it, so a compromised
/// service cannot read the keystroke stream. Publishing onto the bus
/// (`CAP_INPUT_SOURCE`) is stripped too: no service holds it until the USB
/// driver gets a manifest row (`docs/usb-hid-plan.md` U5). `None` (plain inherit) when this
/// task's own credentials cannot be read.
fn manifest_cred(name: &str) -> Option<sys::Cred> {
    let mut own = sys::Cred::default();
    sys::cred_get(None, &mut own).ok()?;
    if name == "inputd" {
        return Some(sys::Cred::new(0, 0, sys::CAP_INPUT_RAW, own.label_id, 0));
    }
    own.caps &= !(sys::CAP_INPUT_RAW | sys::CAP_INPUT_SOURCE);
    Some(own)
}

/// Start one supervised row as a child of this task: stamped with the row's
/// credentials for a launched app, with the manifest credentials (or this
/// supervisor's own identity) for a manifest service.
pub(super) fn spawn_service(
    services: &mut [Service],
    index: usize,
    broker: &mut router::TopicBroker,
) {
    let cred = services[index]
        .cred
        .or_else(|| manifest_cred(services[index].name));
    let spawned = spawn_row(&services[index], services[index].restarts, cred);
    match spawned {
        Some(pid) => {
            services[index].pid = pid;
            services[index].phase = Phase::Running;
            services[index].started_tick = sys::clock();
            services[index].last_status = None;
            sys::write_str(&format!(
                "init: started {} (pid {}, attempt {})\n",
                services[index].name,
                pid,
                services[index].restarts + 1
            ));
            publish_state(
                broker,
                &services[index],
                "running",
                pid,
                services[index].restarts,
                0,
                "",
            );
        }
        None => {
            services[index].pid = 0;
            services[index].last_status = None;
            services[index].restarts += 1;
            if services[index].restarts >= MAX_RESTARTS {
                services[index].phase = Phase::Failed;
                sys::write_str(&format!(
                    "init: spawn {} failed; giving up\n",
                    services[index].name
                ));
                publish_state(
                    broker,
                    &services[index],
                    "failed",
                    0,
                    services[index].restarts,
                    0,
                    "spawn failed",
                );
            } else {
                services[index].phase = Phase::Restarting;
                services[index].next_start = sys::clock() + backoff(services[index].restarts);
                sys::write_str(&format!(
                    "init: spawn {} failed; retrying in {} ticks\n",
                    services[index].name,
                    backoff(services[index].restarts)
                ));
            }
        }
    }
}

/// The `argv` of a spawn: `[path, args..., attempt=<n>]`. Each argument is
/// one item as the row holds it, so a path with spaces stays whole.
pub(super) fn argv(service: &Service, restarts: u64) -> Vec<String> {
    let mut argv = Vec::with_capacity(service.args.len() + 2);
    argv.push(String::from(service.path));
    argv.extend(service.args.iter().cloned());
    argv.push(format!("attempt={}", restarts + 1));
    argv
}

/// Spawn `service` (its [`argv`] for attempt `restarts + 1`) as a child of
/// this task, under its personality, stamped with `cred` (and the row's label,
/// for an installed app) or inheriting this task's identity when `None`.
///
/// A restarted installed app keeps its label: the kernel stamps it again at
/// the spawn, so a crash never launders the sandbox. The environment is the
/// row's own (a launched app's session `HOME`, `USER` and `PATH`, fixed at
/// launch), so a respawn sees the same one.
pub(super) fn spawn_row(service: &Service, restarts: u64, cred: Option<sys::Cred>) -> Option<u64> {
    let argv = argv(service, restarts);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let personality = if service.linux {
        sys::Personality::Linux
    } else {
        sys::Personality::Native
    };
    let stamp = match (cred, service.label) {
        (Some(cred), Some(label)) => sys::SpawnCred::AsLabelled(cred, label),
        (Some(cred), None) => sys::SpawnCred::As(cred),
        (None, _) => sys::SpawnCred::Inherit,
    };
    let env: Vec<&str> = service.env.iter().map(String::as_str).collect();
    sys::spawnv(service.path, &argv, &env, personality, stamp).ok()
}

/// A service exited: apply its restart policy and publish the event.
pub(super) fn child_exited(
    services: &mut [Service],
    pid: u64,
    status: u64,
    broker: &mut router::TopicBroker,
) {
    let Some(index) = services.iter().position(|service| {
        service.pid == pid && matches!(service.phase, Phase::Running | Phase::Stopping)
    }) else {
        // A child that was not a supervised row (or an already-handled exit):
        // nothing to supervise.
        return;
    };
    // During a shutdown nothing restarts: the exit is the stop completing
    // (or a crash on the way down, which is reported but not respawned).
    if services[index].phase == Phase::Stopping || super::shutdown::stopping() {
        stopped_for_shutdown(services, index, status, broker);
        return;
    }
    let name = services[index].name;
    let desired = services[index].restart;
    let launched = services[index].launched;
    let uptime = sys::clock().saturating_sub(services[index].started_tick);
    services[index].pid = 0;
    services[index].last_status = Some(status);
    if uptime >= STABLE_TICKS {
        // A long run wipes the rapid-crash budget.
        services[index].restarts = 0;
    }
    if launched {
        sys::write_str(&format!("INIT:LAUNCH:EXIT app={name} status={status}\n"));
    }
    let restart = restarts_after(desired, status);
    if restart {
        services[index].restarts += 1;
    }
    if restart && services[index].restarts >= MAX_RESTARTS {
        services[index].phase = Phase::Failed;
        sys::write_str(&format!(
            "init: service {} failed after {} rapid restarts\n",
            name, services[index].restarts
        ));
        sys::write_str(&format!(
            "INIT:RESTART:EXHAUSTED name={name} restarts={}\n",
            services[index].restarts
        ));
        publish_state(
            broker,
            &services[index],
            "failed",
            0,
            services[index].restarts,
            status,
            "restart budget exhausted",
        );
    } else if restart {
        let delay = backoff(services[index].restarts);
        let attempt = services[index].restarts;
        services[index].phase = Phase::Restarting;
        services[index].next_start = sys::clock() + delay;
        sys::write_str(&format!(
            "init: service {} exited (status {}); restart in {} ticks (attempt {})\n",
            name,
            status,
            delay,
            attempt + 1
        ));
        // The machine-parseable restart/backoff marker: a crashing supervised
        // app (`/system/bin/flaky`) proves the path in a headless boot.
        sys::write_str(&format!(
            "INIT:RESTART:PASS name={name} status={status} attempt={} delay={delay}\n",
            attempt + 1
        ));
        publish_state(
            broker,
            &services[index],
            "restarting",
            0,
            attempt,
            status,
            "",
        );
    } else if status != 0 {
        // No restart policy, but it did not exit cleanly: that is a failure,
        // not a service that finished its work.
        services[index].phase = Phase::Failed;
        sys::write_str(&format!(
            "init: service {} exited (status {}); not restarting\n",
            name, status
        ));
        publish_state(
            broker,
            &services[index],
            "failed",
            0,
            services[index].restarts,
            status,
            "exited with an error and has no restart policy",
        );
    } else {
        services[index].phase = Phase::Stopped;
        sys::write_str(&format!(
            "init: service {} exited (status {}); not restarting\n",
            name, status
        ));
        publish_state(
            broker,
            &services[index],
            "stopped",
            0,
            services[index].restarts,
            status,
            "",
        );
    }
}

/// Whether a row with restart policy `policy` comes back after exiting with
/// `status`. `Always` covers every exit, including a kill (the desktop shell
/// relies on it: killing LazyShell brings it back).
pub(super) fn restarts_after(policy: Restart, status: u64) -> bool {
    match policy {
        Restart::Always => true,
        Restart::OnFailure => status != 0,
        Restart::Once => false,
    }
}

/// A row's task exited while the machine is shutting down: retire it.
fn stopped_for_shutdown(
    services: &mut [Service],
    index: usize,
    status: u64,
    broker: &mut router::TopicBroker,
) {
    let row = &mut services[index];
    sys::write_str(&format!(
        "init: stopped {} (status {}{})\n",
        row.name,
        status,
        if row.killed { ", killed" } else { "" }
    ));
    row.phase = Phase::Stopped;
    row.pid = 0;
    row.last_status = Some(status);
    publish_state(broker, row, "stopped", 0, row.restarts, status, "shutdown");
}

/// Capped exponential backoff in PIT ticks.
fn backoff(restarts: u64) -> u64 {
    let shift = restarts.saturating_sub(1).min(6);
    (BACKOFF_BASE << shift).min(BACKOFF_MAX)
}

/// The next tick the supervisor must wake at: a due restart, or the regular
/// request-serving poll.
pub(super) fn wake_deadline(services: &[Service], now: u64) -> u64 {
    let mut deadline = now + POLL_TICKS;
    for service in services {
        if service.phase == Phase::Restarting && service.next_start < deadline {
            deadline = service.next_start;
        }
    }
    deadline
}

/// Publish one service state event, retained per service, on the declared
/// `system/events/service/<name>` topic: the typed payload carries the detail
/// `healthd` needs (pid, restart count) without a follow-up query.
pub(super) fn publish_state(
    broker: &mut router::TopicBroker,
    service: &Service,
    state: &str,
    pid: u64,
    restarts: u64,
    status: u64,
    detail: &str,
) {
    // The health topic is derived from the service name via the generated
    // helper; a name the broker would refuse drops the display field rather
    // than the whole event.
    let health = services::health::wire::name_system_health(service.name).unwrap_or_default();
    let event = services::ServiceEvent {
        state: String::from(state),
        pid,
        restarts,
        status,
        health,
        detail: String::from(detail),
    };
    let _ = services::init::wire::publish_system_events_service(broker, service.name, &event);
}
