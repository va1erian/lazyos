//! `init`'s supervision loop helpers: starting ready services, spawning and
//! reaping children, restart backoff, and publishing state events.
//!
//! Split out of `init.rs` (issue #194); a pure move, no behavior change.

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

/// Start one supervised row as a child of this task: `spawn_as` with the
/// row's stamped credentials for a launched app, plain `spawn` (inheriting the
/// supervisor's identity) for a manifest service.
pub(super) fn spawn_service(
    services: &mut [Service],
    index: usize,
    broker: &mut router::TopicBroker,
) {
    let command = command_line(&services[index], services[index].restarts);
    let spawned = match services[index]
        .cred
        .or_else(|| manifest_cred(services[index].name))
    {
        // A restarted installed app keeps its label: the kernel stamps it again
        // at the spawn, so a crash never launders the sandbox.
        Some(cred) => match services[index].label {
            Some(label) => sys::spawn_as_labelled(&command, &cred, label),
            None => sys::spawn_as(&command, &cred),
        },
        None => sys::spawn(&command),
    };
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

/// The NUL-terminated command line for a spawn: `PATH <args> attempt=<n>`.
pub(super) fn command_line(service: &Service, restarts: u64) -> Vec<u8> {
    // The kernel's spawn picks the Linux ABI personality from this prefix.
    let mut line = String::from(if service.linux { "linux:" } else { "" });
    line.push_str(service.path);
    if !service.args.is_empty() {
        line.push(' ');
        line.push_str(&service.args);
    }
    line.push_str(&format!(" attempt={}", restarts + 1));
    let mut bytes = line.into_bytes();
    while bytes.last() == Some(&b' ') {
        bytes.pop();
    }
    bytes.push(0);
    bytes
}

/// A service exited: apply its restart policy and publish the event.
pub(super) fn child_exited(
    services: &mut [Service],
    pid: u64,
    status: u64,
    broker: &mut router::TopicBroker,
) {
    let Some(index) = services
        .iter()
        .position(|service| service.pid == pid && service.phase == Phase::Running)
    else {
        // A child that was not a supervised row (or an already-handled exit):
        // nothing to supervise.
        return;
    };
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
    let restart = match desired {
        Restart::Always => true,
        Restart::OnFailure => status != 0,
        Restart::Once => false,
    };
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
        // app (`FLAKY.ELF`) proves the path in a headless boot.
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
