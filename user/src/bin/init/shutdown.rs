//! `init`'s orderly shutdown and reboot (docs/shutdown.md): the `Shutdown`
//! request, then a non-blocking sequence the supervision loop steps every
//! wakeup, so `init` keeps reaping children and answering requests while it
//! runs.
//!
//! 1. **Freeze** ([`begin`]): restarts, autostart and `Launch` stop
//!    ([`stopping`]); rows waiting to start or restart are retired; the kernel
//!    watchdog is armed so a hang here still ends in a synced stop.
//! 2. **Apps**: every launched app gets `SIGTERM`, then `SIGKILL` at its
//!    deadline.
//! 3. **Services**: the manifest services in [`stop_order`] order. A service
//!    that serves `os.lazy.lifecycle.v1` gets its `Shutdown` message (it
//!    persists and exits); any other gets `SIGTERM`. Each has a deadline, then
//!    `SIGKILL`.
//! 4. **Quiesced**: every child is reaped; `init` publishes the last phase and
//!    calls the kernel's `power`, which syncs the filesystems and stops.
//!
//! A global deadline bounds the whole sequence, and `force` (a second request
//! with it set) jumps straight to killing what is left. Every phase is
//! published retained on `system/power/state` and logged as
//! `INIT:SHUTDOWN:<PHASE>` on serial.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use user::files;
use user::messenger::{self, confd, pkgd, registry, router, services};
use user::sys::{self, Cred as SysCred};

use super::service::{Phase, Service};
use super::stop_order::{self, Node};
use super::supervise::publish_state;

/// How long launched apps get to exit after `SIGTERM` (100 Hz): 5 s.
const APP_STOP_TICKS: u64 = 500;
/// How long one service gets to exit after its stop message: 3 s.
const SERVICE_STOP_TICKS: u64 = 300;
/// How long a killed task gets to be reaped before its row is written off.
const KILL_GRACE_TICKS: u64 = 50;
/// The whole sequence's deadline: 20 s. The kernel watchdog fires at 30 s.
const GLOBAL_TICKS: u64 = 2000;
/// The most bytes of reason text accepted.
const MAX_REASON: usize = 128;

/// The services that serve the lifecycle contract: manifest name, registered
/// name. Everything else is stopped with `SIGTERM`.
const GRACEFUL: &[(&str, &str)] = &[
    ("confd", confd::NAME),
    ("logd", services::LOGD_NAME),
    ("pkgd", pkgd::NAME),
];

/// Services left running into `power`: `usbd` serves the USB stick that may
/// hold `/home`, and the kernel's final sync flushes it through `usbd`
/// (docs/architecture/usb-storage.md). The kernel's watchdog and request
/// timeouts still bound a driver that hangs.
const OUTLIVE: &[&str] = &["usbd"];

/// Whether `row` is left running for the kernel's final sync.
fn outlives(row: &Service) -> bool {
    !row.launched && OUTLIVE.contains(&row.name)
}

/// Set once a shutdown starts; never cleared (the sequence is one-way).
static STOPPING: AtomicBool = AtomicBool::new(false);

/// Whether a shutdown is running: restarts, autostart and `Launch` are off.
pub(super) fn stopping() -> bool {
    STOPPING.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Apps,
    Services,
    Quiesced,
    /// `power` returned (refused): nothing more to do.
    Failed,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Stage::Apps => "apps",
            Stage::Services => "services",
            Stage::Quiesced => "quiesced",
            Stage::Failed => "failed",
        }
    }
}

/// A running shutdown.
pub(super) struct Shutdown {
    mode: u32,
    reason: String,
    stage: Stage,
    /// Whether the apps/services stage has sent its stop requests yet.
    entered: bool,
    started: u64,
    deadline: u64,
    force: bool,
    killed: u64,
}

/// Answer one `Shutdown` request: start the sequence, or report the one
/// already running. Returns the phase for the reply.
pub(super) fn request(
    current: &mut Option<Shutdown>,
    services: &mut [Service],
    broker: &mut router::TopicBroker,
    args: &services::init::wire::ShutdownArgs,
    caller: &SysCred,
) -> messenger::Result<String> {
    authorize(caller)?;
    if args.mode > services::POWER_MODE_REBOOT || !valid_reason(&args.reason) {
        return Err(messenger::Error::Errno(-messenger::errno::EINVAL));
    }
    if let Some(running) = current {
        if args.force && !running.force {
            sys::write_str("INIT:SHUTDOWN:FORCE\n");
            running.force = true;
        }
        return Ok(String::from(running.stage.label()));
    }
    let shutdown = begin(services, broker, args, caller);
    let phase = String::from(shutdown.stage.label());
    *current = Some(shutdown);
    Ok(phase)
}

/// Who may stop the machine: a system service (`CAP_SETUID`), or any caller
/// in a login session. An installed (labelled) app never may, whatever its
/// uid, and neither may a sessionless task without the capability (a driver,
/// the login screen).
fn authorize(caller: &SysCred) -> messenger::Result<()> {
    if caller.label_id == 0 && (caller.caps & super::state::CAP_SETUID != 0 || caller.session != 0) {
        Ok(())
    } else {
        Err(messenger::Error::Errno(-messenger::errno::EPERM))
    }
}

/// A reason is short, printable text: it goes to the log and the topic.
fn valid_reason(reason: &str) -> bool {
    reason.len() <= MAX_REASON && !reason.chars().any(char::is_control)
}

/// Phase 1, the freeze.
fn begin(
    services: &mut [Service],
    broker: &mut router::TopicBroker,
    args: &services::init::wire::ShutdownArgs,
    caller: &SysCred,
) -> Shutdown {
    STOPPING.store(true, Ordering::Relaxed);
    let now = sys::clock();
    let shutdown = Shutdown {
        mode: args.mode,
        reason: args.reason.clone(),
        stage: Stage::Apps,
        entered: false,
        started: now,
        deadline: now + GLOBAL_TICKS,
        force: args.force,
        killed: 0,
    };
    sys::write_str(&format!(
        "INIT:SHUTDOWN:BEGIN mode={} uid={} session={} reason=\"{}\"\n",
        mode_name(args.mode),
        caller.uid,
        caller.session,
        args.reason
    ));
    if let Err(code) = files::power_arm(kernel_op(args.mode)) {
        sys::write_str(&format!(
            "init: shutdown watchdog not armed (errno {code})\n"
        ));
    }
    // Nothing waiting to start or restart will: those rows hold no task.
    for row in services.iter_mut() {
        if matches!(row.phase, Phase::Pending | Phase::Restarting) {
            row.phase = Phase::Stopped;
            publish_state(broker, row, "stopped", 0, row.restarts, 0, "shutdown");
        }
    }
    shutdown.publish(broker, "stopping");
    shutdown
}

impl Shutdown {
    /// Advance the sequence; called every supervision-loop wakeup after the
    /// exits were reaped. A stage that completes enters the next one at once
    /// (the loop parks until an exit or [`Shutdown::next_deadline`] in
    /// between). Only returns once `power` is refused.
    pub(super) fn step(&mut self, services: &mut [Service], broker: &mut router::TopicBroker) {
        loop {
            let (stage, entered) = (self.stage, self.entered);
            self.step_once(services, broker);
            if self.stage == stage && self.entered == entered {
                return;
            }
        }
    }

    /// When the sequence must look again with no exit to wake it: the
    /// earliest stop deadline of a `Stopping` row, or the global deadline.
    /// Nothing is timed once the services are down (`power` ran).
    pub(super) fn next_deadline(&self, services: &[Service]) -> Option<u64> {
        if matches!(self.stage, Stage::Quiesced | Stage::Failed) {
            return None;
        }
        services
            .iter()
            .filter(|row| row.phase == Phase::Stopping)
            .map(|row| row.stop_deadline)
            .chain(core::iter::once(self.deadline))
            .min()
    }

    fn step_once(&mut self, services: &mut [Service], broker: &mut router::TopicBroker) {
        let now = sys::clock();
        if (self.force || now >= self.deadline)
            && matches!(self.stage, Stage::Apps | Stage::Services)
        {
            let why = if self.force {
                "forced"
            } else {
                "deadline passed"
            };
            sys::write_str(&format!("init: shutdown {why}; killing what is left\n"));
            self.kill_all(services, broker, now);
            self.stage = Stage::Quiesced;
        }
        expire(services, broker, now, &mut self.killed);
        match self.stage {
            Stage::Apps => self.step_apps(services, broker, now),
            Stage::Services => self.step_services(services, broker, now),
            Stage::Quiesced => self.power_off(broker, now),
            Stage::Failed => {}
        }
    }

    /// Phase 2: `SIGTERM` every launched app, then wait for all to go.
    fn step_apps(&mut self, services: &mut [Service], broker: &mut router::TopicBroker, now: u64) {
        if !self.entered {
            self.entered = true;
            self.publish(broker, "apps");
            let mut asked = 0;
            for row in services.iter_mut().filter(|row| row.launched) {
                if row.phase == Phase::Running {
                    ask_to_stop(row, broker, sys::SIG_TERM, now + APP_STOP_TICKS);
                    asked += 1;
                }
            }
            sys::write_str(&format!("INIT:SHUTDOWN:APPS asked={asked}\n"));
        }
        if !services.iter().any(|row| row.launched && is_live(row)) {
            self.stage = Stage::Services;
            self.entered = false;
        }
    }

    /// Phase 3: the manifest services, in stop order.
    fn step_services(
        &mut self,
        services: &mut [Service],
        broker: &mut router::TopicBroker,
        now: u64,
    ) {
        if !self.entered {
            self.entered = true;
            self.publish(broker, "services");
            sys::write_str("INIT:SHUTDOWN:SERVICES\n");
        }
        let rows: Vec<usize> = (0..services.len())
            .filter(|&index| !services[index].launched && !outlives(&services[index]))
            .collect();
        let nodes: Vec<Node> = rows
            .iter()
            .map(|&index| Node {
                name: services[index].name,
                deps: services[index].deps,
                tier: stop_order::tier(services[index].name),
                live: is_live(&services[index]),
                stopping: services[index].phase == Phase::Stopping,
            })
            .collect();
        let ready = stop_order::ready(&nodes);
        if ready.relaxed {
            sys::write_str("init: shutdown: dependency order stalled; relaxing it\n");
        }
        for node in ready.rows {
            self.stop_service(&mut services[rows[node]], broker, now);
        }
        if !services.iter().any(|row| is_live(row) && !outlives(row)) {
            self.stage = Stage::Quiesced;
        }
    }

    /// Ask one service to stop: its lifecycle message, or `SIGTERM`.
    fn stop_service(&self, row: &mut Service, broker: &mut router::TopicBroker, now: u64) {
        let graceful = GRACEFUL
            .iter()
            .find(|(name, _)| *name == row.name)
            .is_some_and(|(_, registered)| send_lifecycle(registered, &self.reason));
        let signal = if graceful { None } else { Some(sys::SIG_TERM) };
        sys::write_str(&format!(
            "init: stopping {} ({})\n",
            row.name,
            if graceful { "lifecycle" } else { "SIGTERM" }
        ));
        match signal {
            Some(sig) => ask_to_stop(row, broker, sig, now + SERVICE_STOP_TICKS),
            None => mark_stopping(row, broker, now + SERVICE_STOP_TICKS),
        }
    }

    /// Phase 5 and 6: everything is reaped; hand over to the kernel.
    fn power_off(&mut self, broker: &mut router::TopicBroker, now: u64) {
        sys::write_str(&format!(
            "init: userspace quiesced (killed={})\nINIT:SHUTDOWN:QUIESCED killed={} ticks={}\n",
            self.killed,
            self.killed,
            now.saturating_sub(self.started)
        ));
        self.publish(broker, "power");
        sys::write_str(&format!(
            "INIT:SHUTDOWN:POWER mode={}\n",
            mode_name(self.mode)
        ));
        if let Err(code) = files::power(kernel_op(self.mode)) {
            sys::write_str(&format!("INIT:SHUTDOWN:FAIL power errno={code}\n"));
            self.publish(broker, "failed");
            self.stage = Stage::Failed;
        }
    }

    /// `SIGKILL` every row still holding a task and write the rows off.
    fn kill_all(&mut self, services: &mut [Service], broker: &mut router::TopicBroker, now: u64) {
        for row in services
            .iter_mut()
            .filter(|row| is_live(row) && !outlives(row))
        {
            if row.pid != 0 && !row.killed {
                let _ = sys::kill(row.pid, sys::SIG_KILL);
                self.killed += 1;
            }
            retire(row, broker, "killed at the shutdown deadline");
        }
        // Reap what the kills ended, so no zombie outlives the stop.
        while sys::wait(now + 1).is_some() {}
    }

    /// Publish the phase on `system/power/state` and log it.
    fn publish(&self, broker: &mut router::TopicBroker, phase: &str) {
        let state = services::PowerState {
            phase: String::from(phase),
            mode: self.mode,
            reason: self.reason.clone(),
            deadline: self.deadline,
        };
        let _ = services::init::wire::publish_system_power_state(broker, &state);
        sys::write_str(&format!("INIT:SHUTDOWN:PHASE {phase}\n"));
    }
}

/// Kill every `Stopping` row past its deadline; write off a killed one that
/// was never reaped.
fn expire(services: &mut [Service], broker: &mut router::TopicBroker, now: u64, killed: &mut u64) {
    for row in services.iter_mut() {
        if row.phase != Phase::Stopping || now < row.stop_deadline {
            continue;
        }
        if row.killed {
            sys::write_str(&format!("init: {} not reaped after SIGKILL\n", row.name));
            retire(row, broker, "not reaped after SIGKILL");
            continue;
        }
        sys::write_str(&format!("init: {} killed (stop deadline)\n", row.name));
        let _ = sys::kill(row.pid, sys::SIG_KILL);
        row.killed = true;
        row.stop_deadline = now + KILL_GRACE_TICKS;
        *killed += 1;
    }
}

/// Send `signal` and mark the row `Stopping` until `deadline`.
fn ask_to_stop(row: &mut Service, broker: &mut router::TopicBroker, signal: u64, deadline: u64) {
    match sys::kill(row.pid, signal) {
        // Already gone: its exit is queued for the supervision loop.
        Ok(()) | Err(-3) => {}
        Err(code) => sys::write_str(&format!(
            "init: signal {} to {} failed (errno {code})\n",
            signal, row.name
        )),
    }
    mark_stopping(row, broker, deadline);
}

fn mark_stopping(row: &mut Service, broker: &mut router::TopicBroker, deadline: u64) {
    row.phase = Phase::Stopping;
    row.stop_deadline = deadline;
    publish_state(
        broker,
        row,
        "stopping",
        row.pid,
        row.restarts,
        0,
        "shutdown",
    );
}

/// A row whose task is gone (or given up on): `Stopped`, never restarted.
fn retire(row: &mut Service, broker: &mut router::TopicBroker, detail: &str) {
    row.phase = Phase::Stopped;
    row.pid = 0;
    publish_state(broker, row, "stopped", 0, row.restarts, 0, detail);
}

/// A row that still holds a task.
fn is_live(row: &Service) -> bool {
    matches!(row.phase, Phase::Running | Phase::Stopping)
}

/// Send the lifecycle `Shutdown` to the service registered as `name`; `false`
/// when it cannot be reached (the caller falls back to `SIGTERM`).
fn send_lifecycle(name: &str, reason: &str) -> bool {
    let Ok(endpoint) = registry::resolve(name) else {
        return false;
    };
    let sent = services::lifecycle::send_shutdown(&endpoint, reason).is_ok();
    let _ = endpoint.close();
    sent
}

/// The kernel `power` op for a `PowerMode`.
fn kernel_op(mode: u32) -> u64 {
    if mode == services::POWER_MODE_REBOOT {
        files::POWER_REBOOT
    } else {
        files::POWER_OFF
    }
}

fn mode_name(mode: u32) -> &'static str {
    if mode == services::POWER_MODE_REBOOT {
        "reboot"
    } else {
        "poweroff"
    }
}
