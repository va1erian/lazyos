//! `init` (`SUPER.ELF`): the userspace service supervisor (issue #93).
//!
//! This is the S2 supervisor from `docs/messenger.md` section 8 and the
//! platform plan section 4.3. It boots as a normal ring-3 program and owns the
//! system from there:
//!
//! * it registers [`services::INIT_NAME`] and serves its supervision table
//!   (`Services`) plus a [`router::TopicBroker`] on the same endpoint;
//! * it starts services from [`MANIFEST`] in dependency order, through the
//!   native `spawn` syscall (syscall 6), which makes each service a child of
//!   this task;
//! * it waits for child exits with the native `wait` syscall (syscall 7) and
//!   restarts a crashed service after a capped exponential backoff, giving up
//!   after [`MAX_RESTARTS`] rapid crashes;
//! * every state change is published retained on
//!   `system/events/service/<name>`, so `logd` (and any subscriber) sees it,
//!   and dependencies only start once their dependencies are running.
//!
//! The manifest is a static Rust table today. Each row carries the fields the
//! issue asks for: name, FAT path, argument string, restart policy, dependency
//! names and health topic. The supervisor appends `attempt=<n>` to the
//! argument string on every spawn, so a service can distinguish a restart; that
//! is how `FLAKY.ELF` crashes exactly once.
//!
//! Boot it with `LAZYOS_SERVICES=1` (see the kernel build script).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::messenger::{self, registry, router, services, Endpoint, Message, Parcel};
use user::sys;

/// Serve subscriptions and due restarts at least this often (PIT ticks).
const POLL_TICKS: u64 = 5;
/// First restart delay (PIT ticks, 100 Hz), doubled per rapid crash.
const BACKOFF_BASE: u64 = 10;
/// Restart delay cap, so a crash loop stays gentle.
const BACKOFF_MAX: u64 = 300;
/// A service that stayed up this long is considered recovered: its restart
/// counter resets, so occasional crashes never exhaust the budget.
const STABLE_TICKS: u64 = 100;
/// Give up restarting a service after this many *rapid* crashes.
const MAX_RESTARTS: u64 = 5;

/// What to do when a service exits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Restart {
    /// Restart on any exit.
    Always,
    /// Restart only when the exit status is non-zero.
    OnFailure,
    /// Never restart.
    Once,
}

/// One manifest row: the fields the supervisor needs to start and watch a
/// service. `health_topic` is the retained topic `healthd` publishes for it.
struct ServiceSpec {
    name: &'static str,
    path: &'static str,
    args: &'static str,
    restart: Restart,
    deps: &'static [&'static str],
    health_topic: &'static str,
}

/// The boot manifest. `messengerd` is first because it owns the bootstrap
/// registry listener; `keyd`, `logd` and `healthd` depend on it; `flaky`
/// depends on `healthd` so the crash test also proves dependency gating.
/// `messengerd` is `Once` because the kernel's bootstrap channel can be
/// claimed only once per boot, so restarting it could not re-listen.
const MANIFEST: &[ServiceSpec] = &[
    ServiceSpec {
        name: "messengerd",
        path: "MSGRD.ELF",
        args: "",
        restart: Restart::Once,
        deps: &[],
        health_topic: "system/health/messengerd",
    },
    ServiceSpec {
        name: "keyd",
        path: "KEYD.ELF",
        args: "",
        restart: Restart::Always,
        deps: &["messengerd"],
        health_topic: "system/health/keyd",
    },
    ServiceSpec {
        name: "logd",
        path: "LOGD.ELF",
        args: "",
        restart: Restart::Always,
        deps: &["messengerd"],
        health_topic: "system/health/logd",
    },
    ServiceSpec {
        name: "healthd",
        path: "HEALTHD.ELF",
        args: "",
        restart: Restart::Always,
        deps: &["messengerd"],
        health_topic: "system/health/healthd",
    },
    ServiceSpec {
        name: "flaky",
        path: "FLAKY.ELF",
        args: "",
        restart: Restart::OnFailure,
        deps: &["healthd"],
        health_topic: "system/health/flaky",
    },
];

/// Runtime phase of a service; `label` is the word published in events and
/// shown by `messengerctl services`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Waiting for its dependencies.
    Pending,
    /// Started and not yet reaped.
    Running,
    /// Exited; a restart is scheduled at `next_start`.
    Restarting,
    /// Exited and will not be restarted.
    Stopped,
    /// Gave up after too many rapid crashes.
    Failed,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Phase::Pending => "pending",
            Phase::Running => "running",
            Phase::Restarting => "restarting",
            Phase::Stopped => "stopped",
            Phase::Failed => "failed",
        }
    }

    /// The health word this phase implies for the `Services` table.
    fn health(self) -> &'static str {
        match self {
            Phase::Running => "ok",
            Phase::Restarting => "degraded",
            _ => "down",
        }
    }
}

/// One supervised service instance.
struct Service {
    spec: &'static ServiceSpec,
    phase: Phase,
    /// Task slot of the running child; `0` between runs.
    pid: u64,
    /// Rapid-crash counter (reset once a run is [`STABLE_TICKS`] old).
    restarts: u64,
    /// Tick the current run started at.
    started_tick: u64,
    /// Absolute tick the next restart is due at.
    next_start: u64,
    /// Exit status of the last run.
    last_status: Option<u64>,
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("init: supervisor starting (issue #93)\n");
    if let Err(error) = run() {
        sys::write_str("init: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

/// Register the supervisor, start the manifest, and run the supervision loop.
fn run() -> messenger::Result<()> {
    // The endpoint clients resolve: it serves the topic broker and the
    // supervision table. Registering the published side and serving the other
    // is the same split `Bus::subscribe` uses for a sink.
    let (published, server) = messenger::create_pair()?;
    registry::register(
        services::INIT_NAME,
        &published,
        &[services::INIT_INTERFACE, router::INTERFACE],
        0,
    )?;
    let mut broker = router::TopicBroker::new("os.lazy.events.sink");
    let mut services: Vec<Service> = MANIFEST
        .iter()
        .map(|spec| Service {
            spec,
            phase: Phase::Pending,
            pid: 0,
            restarts: 0,
            started_tick: 0,
            next_start: 0,
            last_status: None,
        })
        .collect();
    sys::write_str(&format!("init: manifest: {} service(s)\n", services.len()));
    start_ready(&mut services, &mut broker);
    // One receive buffer for the whole life of the supervisor: the user bump
    // allocator never reclaims per-call buffers, so long-lived loops must not
    // allocate one per request.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    let mut cache = StatusCache::default();

    loop {
        // A restart whose backoff elapsed.
        let now = sys::clock();
        for index in 0..services.len() {
            if services[index].phase == Phase::Restarting && services[index].next_start <= now {
                spawn_service(&mut services, index, &mut broker);
            }
        }
        // Reap one exit (or time out to serve requests).
        if let Some((pid, status)) = sys::wait(wake_deadline(&services, now)) {
            child_exited(&mut services, pid, status, &mut broker);
            // The exit may unblock dependents (only a stop can; still cheap).
            start_ready(&mut services, &mut broker);
        }
        serve_pending(&mut services, &mut broker, &server, &mut buffer, &mut cache)?;
    }
}

/// Cached `Services` reply.
///
/// The user runtime's bump allocator never reclaims memory, and every encoded
/// reply allocates several small buffers, so the supervisor re-encodes its
/// supervision table only when a cheap fingerprint (phase/pid/restarts) says
/// it changed; callers get a clone of the encoded parcel.
#[derive(Default)]
struct StatusCache {
    fingerprint: Option<u64>,
    parcel: Option<Parcel>,
}

impl StatusCache {
    /// The current reply, re-encoding the table when it changed.
    fn parcel(&mut self, services: &[Service]) -> messenger::Result<Parcel> {
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut fingerprint = 0xcbf2_9ce4_8422_2325u64;
        for service in services {
            for byte in service
                .phase
                .label()
                .bytes()
                .chain(service.pid.to_le_bytes())
                .chain(service.restarts.to_le_bytes())
            {
                fingerprint ^= byte as u64;
                fingerprint = fingerprint.wrapping_mul(PRIME);
            }
        }
        if self.fingerprint != Some(fingerprint) || self.parcel.is_none() {
            self.parcel = Some(services::services_reply(&status_rows(services))?);
            self.fingerprint = Some(fingerprint);
        }
        Ok(self.parcel.clone().unwrap_or_default())
    }
}

/// Start every `Pending` service whose dependencies are `Running`, repeating
/// until no more can start (a single pass suffices for an ordered manifest,
/// but this is order-independent).
fn start_ready(services: &mut [Service], broker: &mut router::TopicBroker) {
    loop {
        let mut started = false;
        for index in 0..services.len() {
            if services[index].phase != Phase::Pending {
                continue;
            }
            let spec = services[index].spec;
            let ready = spec.deps.iter().all(|dep| {
                services
                    .iter()
                    .any(|service| service.spec.name == *dep && service.phase == Phase::Running)
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

/// Start one service as a child of this task.
fn spawn_service(services: &mut [Service], index: usize, broker: &mut router::TopicBroker) {
    let spec = services[index].spec;
    let command = command_line(spec, services[index].restarts);
    match sys::spawn(&command) {
        Some(pid) => {
            services[index].pid = pid;
            services[index].phase = Phase::Running;
            services[index].started_tick = sys::clock();
            services[index].last_status = None;
            sys::write_str(&format!(
                "init: started {} (pid {}, attempt {})\n",
                spec.name,
                pid,
                services[index].restarts + 1
            ));
            publish_state(
                broker,
                services[index].spec,
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
                sys::write_str(&format!("init: spawn {} failed; giving up\n", spec.name));
                publish_state(
                    broker,
                    spec,
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
                    spec.name,
                    backoff(services[index].restarts)
                ));
            }
        }
    }
}

/// The NUL-terminated command line for a spawn: `PATH <args> attempt=<n>`.
fn command_line(spec: &ServiceSpec, restarts: u64) -> Vec<u8> {
    let mut line = format!("{} {}", spec.path, spec.args);
    line.push_str(&format!("attempt={}", restarts + 1));
    let mut bytes = line.into_bytes();
    while bytes.last() == Some(&b' ') {
        bytes.pop();
    }
    bytes.push(0);
    bytes
}

/// A service exited: apply its restart policy and publish the event.
fn child_exited(services: &mut [Service], pid: u64, status: u64, broker: &mut router::TopicBroker) {
    let Some(index) = services
        .iter()
        .position(|service| service.pid == pid && service.phase == Phase::Running)
    else {
        // A child that was not a manifest service (or an already-handled
        // exit): nothing to supervise.
        return;
    };
    let spec = services[index].spec;
    let uptime = sys::clock().saturating_sub(services[index].started_tick);
    services[index].pid = 0;
    services[index].last_status = Some(status);
    if uptime >= STABLE_TICKS {
        // A long run wipes the rapid-crash budget.
        services[index].restarts = 0;
    }
    let restart = match spec.restart {
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
            spec.name, services[index].restarts
        ));
        publish_state(
            broker,
            spec,
            "failed",
            0,
            services[index].restarts,
            status,
            "restart budget exhausted",
        );
    } else if restart {
        let delay = backoff(services[index].restarts);
        services[index].phase = Phase::Restarting;
        services[index].next_start = sys::clock() + delay;
        sys::write_str(&format!(
            "init: service {} exited (status {}); restart in {} ticks (attempt {})\n",
            spec.name,
            status,
            delay,
            services[index].restarts + 1
        ));
        publish_state(
            broker,
            spec,
            "restarting",
            0,
            services[index].restarts,
            status,
            "",
        );
    } else {
        services[index].phase = Phase::Stopped;
        sys::write_str(&format!(
            "init: service {} exited (status {}); not restarting\n",
            spec.name, status
        ));
        publish_state(
            broker,
            spec,
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
fn wake_deadline(services: &[Service], now: u64) -> u64 {
    let mut deadline = now + POLL_TICKS;
    for service in services {
        if service.phase == Phase::Restarting && service.next_start < deadline {
            deadline = service.next_start;
        }
    }
    deadline
}

/// Publish one service state event, retained per service:
/// `system/events/service/<name>` with a `key=value` payload that carries the
/// detail `healthd` needs (pid, restart count) without a follow-up query.
fn publish_state(
    broker: &mut router::TopicBroker,
    spec: &ServiceSpec,
    state: &str,
    pid: u64,
    restarts: u64,
    status: u64,
    detail: &str,
) {
    let topic = format!("system/events/service/{}", spec.name);
    let mut payload = format!(
        "state={state} pid={pid} restarts={restarts} status={status} health={}",
        spec.health_topic
    );
    if !detail.is_empty() {
        payload.push_str(&format!(" detail={detail}"));
    }
    broker.publish(&topic, payload.as_bytes(), true);
}

/// Serve queued subscriptions and control calls without blocking.
fn serve_pending(
    services: &mut [Service],
    broker: &mut router::TopicBroker,
    server: &Endpoint,
    buffer: &mut [u8],
    cache: &mut StatusCache,
) -> messenger::Result<()> {
    while let Some(message) = server.poll_recv_with(buffer)? {
        let reply = match dispatch(services, broker, &message, cache) {
            Ok(parcel) => parcel,
            // A malformed request still gets an answer, or its caller would
            // wait forever. An empty table is a valid `Services` reply.
            Err(_) => services::services_reply(&[]).unwrap_or_default(),
        };
        if let Some(txn) = message.txn {
            server.reply(txn, &reply)?;
        }
    }
    Ok(())
}

/// Dispatch one inbound message to the broker or the supervision table.
fn dispatch(
    services: &mut [Service],
    broker: &mut router::TopicBroker,
    message: &Message,
    cache: &mut StatusCache,
) -> messenger::Result<Parcel> {
    match message.interface_id() {
        router::INTERFACE => broker.handle(message),
        services::INIT_INTERFACE => match message.method() {
            services::init_method::SERVICES => cache.parcel(services),
            _ => Err(messenger::Error::Errno(-messenger::errno::EINVAL)),
        },
        _ => Err(messenger::Error::Errno(-messenger::errno::EINVAL)),
    }
}

/// Snapshot the supervision table as wire rows.
fn status_rows(services: &[Service]) -> Vec<services::ServiceStatus> {
    services
        .iter()
        .map(|service| {
            let deps = service
                .spec
                .deps
                .iter()
                .fold(String::new(), |mut text, dep| {
                    if !text.is_empty() {
                        text.push(',');
                    }
                    text.push_str(dep);
                    text
                });
            services::ServiceStatus {
                name: service.spec.name.to_string(),
                state: service.phase.label().to_string(),
                pid: service.pid,
                restarts: service.restarts,
                deps,
                health: service.phase.health().to_string(),
            }
        })
        .collect()
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
