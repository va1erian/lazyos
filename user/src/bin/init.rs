//! `init` (`SUPER.ELF`): the userspace service supervisor (issue #93) and the
//! app-launch path (issue #158).
//!
//! This is the S2 supervisor from `docs/messenger.md` section 8 and the
//! platform plan section 4.3. It boots as a normal ring-3 program and owns the
//! system from there:
//!
//! * it registers [`services::INIT_NAME`] and serves its supervision table
//!   (`Services`), its built-in app registry (`ListApps`) and its launch call
//!   (`Launch`) plus a [`router::TopicBroker`] on the same endpoint;
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
//! # App launch and the registry
//!
//! The built-in [`APPS`] table maps an **app id** (the lowercase program stem,
//! `top` -> `TOP.ELF`) to a display name, its ELF path, a default restart
//! policy and the MIME verbs it handles. `ListApps` serves it to the S5 start
//! menu, and `mimed`'s open-with registrations resolve to the same ids.
//!
//! `Launch(app_id, args, session)` spawns *the target session's child* with
//! `spawn_as`, so the kernel stamps uid/gid/session before the app runs, and
//! then supervises it exactly like a manifest service: the same restart policy,
//! crash backoff, `system/health/<name>` and `system/events/service/<name>`.
//! `session` 0 means the caller's own session. The policy is session-owner
//! only: a task may launch into its own session; root (or a task holding
//! `CAP_SETUID`) may launch anywhere; anyone else is refused with `-EPERM`
//! (printed as `INIT:LAUNCH:DENIED:PASS` and answered with a structured
//! error). Root launching into a *different* session resolves the session's
//! uid/gid from `logind`.
//!
//! A session may hold at most [`LAUNCH_CAP_PER_SESSION`] launched rows
//! reserved at once (issue #177): each `Launch` call spawns a fresh row and
//! only a `Stopped`/`Failed` row for the same app is ever superseded, so
//! nothing else stopped an unprivileged caller from looping `launch` until
//! the 16-slot task table (`kernel/src/task/mod.rs`'s `MAX_TASKS`) was full,
//! starving supervised restarts and new logins. A request over the cap is
//! refused with `-EAGAIN` (`INIT:LAUNCH:CAP:PASS`) before anything spawns.
//! The reservation counts `Running` rows and any row still cycling through
//! crash backoff (`Restarting`/`Pending`), since those respawn from the
//! supervision loop without another cap check ([`running_in_session`]).
//!
//! Boot evidence: `INIT:APPS:PASS`, `INIT:LAUNCH:PASS` (the self-test launches
//! `TOP.ELF`; the app's own `SYS:TOP:PASS` and exit prove it ran),
//! `INIT:LAUNCH:DENIED:PASS` (the policy self-test) and `INIT:LAUNCH:CAP:PASS`
//! (the concurrency-cap self-test); supervised restarts print
//! `INIT:RESTART:PASS`.
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
use user::messenger::{self, logind, registry, router, services, Endpoint, Message, Parcel};
use user::sys::{self, Cred as SysCred};

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
/// Capabilities a launched session child receives. Empty today, matching
/// `logind`'s session set: least privilege is the default and the S5.2
/// session grants arrive through the credential gate.
const SESSION_CAPS: u32 = 0;
/// `CAP_SETUID` (kernel `ipc::credentials`): a supervisor holding it may
/// launch into any session.
const CAP_SETUID: u32 = 1 << 6;
/// Ticks after boot before the launch self-test first tries (100 Hz). The
/// manifest's `Once` services (top) exit around here, freeing their slots.
const LAUNCH_SELFTEST_DELAY: u64 = 30;
/// Ticks between launch self-test attempts while no slot is free.
const LAUNCH_SELFTEST_RETRY: u64 = 25;
/// Give up on the launch self-test after this many attempts.
const LAUNCH_SELFTEST_ATTEMPTS: u64 = 40;
/// Launched rows one session may hold reserved at once (issue #177). The
/// boot manifest's own services already run the 16-slot task table
/// (`kernel/src/task/mod.rs`'s `MAX_TASKS`) close to full for the life of the
/// boot, so the cap is a small fixed number rather than derived from the
/// live table: it must hold room for supervised restarts and new logins even
/// when nothing else has freed a slot yet. A row still reserves its slot
/// while `Restarting`: [`spawn_service`] respawns it from the main loop's
/// backoff sweep, not through [`launch`], so a crashed row that stopped
/// counting here could let a session accumulate more rows than the cap once
/// they all came back up. [`running_in_session`] counts every phase that
/// currently holds or will reclaim a slot without another cap check.
const LAUNCH_CAP_PER_SESSION: usize = 2;

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

impl Restart {
    /// The wire word `ListApps` reports.
    fn label(self) -> &'static str {
        match self {
            Restart::Always => "always",
            Restart::OnFailure => "on-failure",
            Restart::Once => "once",
        }
    }
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

/// One app-registry row (issue #158): what the start menu enumerates and what
/// `Launch` resolves an app id to.
struct AppSpec {
    /// Lowercase program stem (`top`, `editor`); `mimed` registers these ids.
    id: &'static str,
    /// Display name for menus.
    name: &'static str,
    /// On-disk ELF path (8.3 on the FAT boot image).
    path: &'static str,
    /// Default restart policy for launches.
    restart: Restart,
    /// MIME verbs the app handles.
    verbs: &'static [&'static str],
}

/// The built-in app registry. The first four ids are exactly the ones
/// `mimed`'s open-with defaults register (`editor`, `files`, `viewer`,
/// `runner`), so an `Open` resolution names an app the supervisor knows; the
/// rest are launchable system programs (`top` proves the path end to end in a
/// headless boot).
const APPS: &[AppSpec] = &[
    AppSpec {
        id: "editor",
        name: "Editor",
        path: "EDITOR.ELF",
        restart: Restart::OnFailure,
        verbs: &["open", "edit"],
    },
    AppSpec {
        id: "files",
        name: "Files",
        path: "FILES.ELF",
        restart: Restart::OnFailure,
        verbs: &["open", "reveal"],
    },
    AppSpec {
        id: "viewer",
        name: "Image Viewer",
        path: "VIEW.ELF",
        restart: Restart::OnFailure,
        verbs: &["open", "reveal"],
    },
    AppSpec {
        id: "runner",
        name: "Program Runner",
        path: "RUNNER.ELF",
        restart: Restart::Once,
        verbs: &["open"],
    },
    AppSpec {
        id: "terminal",
        name: "Terminal",
        path: "SH.ELF",
        restart: Restart::OnFailure,
        verbs: &["open"],
    },
    AppSpec {
        id: "top",
        name: "System Monitor",
        path: "TOP.ELF",
        restart: Restart::Once,
        verbs: &["open"],
    },
    AppSpec {
        id: "messengerctl",
        name: "Messenger Console",
        path: "MSGCTL.ELF",
        restart: Restart::OnFailure,
        verbs: &[],
    },
];

/// The app registry row for `id` (case-insensitive), if any.
fn find_app(id: &str) -> Option<&'static AppSpec> {
    APPS.iter()
        .find(|app| app.id.eq_ignore_ascii_case(id.trim()))
}

/// The registry as wire rows for `ListApps`.
fn app_infos() -> Vec<services::AppInfo> {
    APPS.iter()
        .map(|app| services::AppInfo {
            id: app.id.to_string(),
            name: app.name.to_string(),
            path: app.path.to_string(),
            restart: app.restart.label().to_string(),
            verbs: app.verbs.iter().map(|verb| verb.to_string()).collect(),
        })
        .collect()
}

/// The boot manifest. `messengerd` is first because it owns the bootstrap
/// registry listener; `keyd`, `logd` and `healthd` depend on it. `accountsd`
/// and `logind` only need the kernel's name registry (which every task can use
/// directly), and `logind` declares its dependency on `accountsd` so the
/// supervisor starts login once accounts are up. `flaky` depends on `healthd`
/// so the crash test also proves dependency gating. `messengerd` is `Once`
/// because the kernel's bootstrap channel can be claimed only once per boot, so
/// restarting it could not re-listen. (`logind`'s console dialog shows in
/// `init`'s window: the kernel routes a child's terminal to its root ancestor,
/// so the supervisor's window carries the login prompt.)
const MANIFEST: &[ServiceSpec] = &[
    ServiceSpec {
        name: "messengerd",
        // `soak=4096` drives a boot-time request/reply self-test through the
        // daemon's serve loop and prints `MSGRD:SOAK`/`MSGRD:TOPICS` evidence
        // (issue #169); it costs a fraction of a second and doubles as a
        // liveness check.
        path: "MSGRD.ELF",
        args: "soak=4096",
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
        name: "accountsd",
        path: "ACCTD.ELF",
        args: "",
        restart: Restart::Always,
        deps: &[],
        health_topic: "system/health/accountsd",
    },
    ServiceSpec {
        name: "logind",
        path: "LOGIND.ELF",
        args: "",
        restart: Restart::Always,
        deps: &["accountsd"],
        health_topic: "system/health/logind",
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
    // The per-session clipboard service (issue #115). It only needs the
    // kernel's name registry, so it depends on nothing. `history=1` is the
    // default one-offer policy; `history=N` keeps up to the service's cap.
    // `demo=1` makes it spawn the two clipboard demo clients (`CLIPCP.ELF`,
    // `CLIPPS.ELF`) at startup; they are evidence programs, not supervised
    // services, so they are spawned and reaped by `clipboardd` instead of
    // adding two more ELF loads to this manifest's boot pass.
    ServiceSpec {
        name: "clipboardd",
        path: "CLIPD.ELF",
        args: "history=1 demo=1",
        restart: Restart::Always,
        deps: &[],
        health_topic: "system/health/clipboardd",
    },
    // `mimed` is the MIME database and open-with registry (issue #116). It
    // depends only on the kernel name registry, which every task can use
    // directly; it talks to `init`'s topic router to publish launch events.
    ServiceSpec {
        name: "mimed",
        path: "MIMED.ELF",
        args: "",
        restart: Restart::Always,
        deps: &[],
        health_topic: "system/health/mimed",
    },
    ServiceSpec {
        name: "flaky",
        path: "FLAKY.ELF",
        args: "",
        restart: Restart::OnFailure,
        deps: &["healthd"],
        health_topic: "system/health/flaky",
    },
    // The system monitor (issue #144): `sysmond` wraps the kernel's
    // system-stats syscall as `os.lazy.system.v1` and republishes retained
    // `system/stats/*` topics. It needs only the kernel name registry, like
    // `mimed`. `demo=1` makes it spawn `top` (`TOP.ELF`), its one-shot
    // evidence client, so a headless boot records `SYS:TOP:PASS`. `top` exits
    // as soon as it has printed its verdict, so it is not a service: listed
    // here it would sit `stopped` and `healthd` would report it `down`
    // forever. `sysmond` spawns and reaps it instead, like `clipboardd`'s demo
    // pair.
    ServiceSpec {
        name: "sysmond",
        path: "SYSD.ELF",
        args: "demo=1",
        restart: Restart::Always,
        deps: &[],
        health_topic: "system/health/sysmond",
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

/// One supervised service instance. Manifest rows and launched apps share the
/// same record; a launched row carries the stamped credentials and the app's
/// runtime argument string.
struct Service {
    /// Name published in events and shown by `messengerctl services`.
    name: &'static str,
    /// On-disk ELF path.
    path: &'static str,
    /// Argument string (manifest default, or the `Launch` request's args).
    args: String,
    /// Restart policy applied to exits.
    restart: Restart,
    /// Service names that must be `Running` before this row starts.
    deps: &'static [&'static str],
    /// Retained health topic for the row.
    health_topic: String,
    /// Credentials for `spawn_as`; `None` inherits this supervisor's identity
    /// (the manifest path).
    cred: Option<SysCred>,
    /// Whether the row came from `Launch` (vs the boot manifest).
    launched: bool,
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

impl Service {
    /// A row for one manifest entry (spawned with this task's identity).
    fn from_manifest(spec: &'static ServiceSpec) -> Service {
        Service {
            name: spec.name,
            path: spec.path,
            args: spec.args.to_string(),
            restart: spec.restart,
            deps: spec.deps,
            health_topic: spec.health_topic.to_string(),
            cred: None,
            launched: false,
            phase: Phase::Pending,
            pid: 0,
            restarts: 0,
            started_tick: 0,
            next_start: 0,
            last_status: None,
        }
    }

    /// A row for one app launch, stamped with the target session's credentials.
    fn from_app(app: &'static AppSpec, args: &str, cred: SysCred) -> Service {
        Service {
            name: app.id,
            path: app.path,
            args: args.to_string(),
            restart: app.restart,
            deps: &[],
            health_topic: format!("system/health/{}", app.id),
            cred: Some(cred),
            launched: true,
            phase: Phase::Pending,
            pid: 0,
            restarts: 0,
            started_tick: 0,
            next_start: 0,
            last_status: None,
        }
    }
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
    let mut services: Vec<Service> = MANIFEST.iter().map(Service::from_manifest).collect();
    sys::write_str(&format!("init: manifest: {} service(s)\n", services.len()));
    selftest_apps();
    selftest_launch_policy();
    selftest_launch_cap();
    start_ready(&mut services, &mut broker);
    // One receive buffer for the whole life of the supervisor: the user bump
    // allocator never reclaims per-call buffers, so long-lived loops must not
    // allocate one per request.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    let mut cache = StatusCache::default();
    let mut selftest = LaunchSelftest::new();

    loop {
        // A restart whose backoff elapsed.
        let now = sys::clock();
        for index in 0..services.len() {
            if services[index].phase == Phase::Restarting && services[index].next_start <= now {
                spawn_service(&mut services, index, &mut broker);
            }
        }
        // The boot launch self-test: spawn `TOP.ELF` through the real launch
        // path once a task slot is free (the manifest's one-shot `top` exits
        // around here), proving `Launch` end to end in a headless boot.
        selftest.step(&mut services, &mut broker, now);
        // Reap one exit (or time out to serve requests).
        if let Some((pid, status)) = sys::wait(wake_deadline(&services, now)) {
            child_exited(&mut services, pid, status, &mut broker);
            // The exit may unblock dependents (only a stop can; still cheap).
            start_ready(&mut services, &mut broker);
        }
        serve_pending(&mut services, &mut broker, &server, &mut buffer, &mut cache)?;
    }
}

/// The boot launch self-test: one `Launch("top")` into this supervisor's own
/// session, retried while the task table is full. `top` is `Once`, so it exits
/// after its own `SYS:TOP:PASS`, which proves the launched app really ran.
struct LaunchSelftest {
    due: u64,
    attempts: u64,
    done: bool,
}

impl LaunchSelftest {
    fn new() -> LaunchSelftest {
        LaunchSelftest {
            due: sys::clock() + LAUNCH_SELFTEST_DELAY,
            attempts: 0,
            done: false,
        }
    }

    /// One attempt when due; schedules the next retry on a full task table.
    fn step(&mut self, services: &mut Vec<Service>, broker: &mut router::TopicBroker, now: u64) {
        if self.done || now < self.due {
            return;
        }
        self.attempts += 1;
        let caller = SysCred::new(0, 0, CAP_SETUID, 0, 0);
        let request = services::LaunchRequest {
            app: String::from("top"),
            args: String::new(),
            session: 0,
        };
        match launch(services, broker, &request, &caller) {
            Ok(_) => self.done = true,
            Err(_) if self.attempts >= LAUNCH_SELFTEST_ATTEMPTS => {
                self.done = true;
                sys::write_str("INIT:LAUNCH:FAIL app=top (spawn unavailable)\n");
            }
            Err(_) => self.due = now + LAUNCH_SELFTEST_RETRY,
        }
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
/// but this is order-independent). Launched app rows are spawned by `launch`,
/// never here (they have no dependencies).
fn start_ready(services: &mut [Service], broker: &mut router::TopicBroker) {
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

/// Start one supervised row as a child of this task: `spawn_as` with the
/// row's stamped credentials for a launched app, plain `spawn` (inheriting the
/// supervisor's identity) for a manifest service.
fn spawn_service(services: &mut [Service], index: usize, broker: &mut router::TopicBroker) {
    let command = command_line(&services[index], services[index].restarts);
    let spawned = match services[index].cred {
        Some(cred) => sys::spawn_as(&command, &cred),
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
fn command_line(service: &Service, restarts: u64) -> Vec<u8> {
    let mut line = String::from(service.path);
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
fn child_exited(services: &mut [Service], pid: u64, status: u64, broker: &mut router::TopicBroker) {
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
    service: &Service,
    state: &str,
    pid: u64,
    restarts: u64,
    status: u64,
    detail: &str,
) {
    let topic = format!("system/events/service/{}", service.name);
    let mut payload = format!(
        "state={state} pid={pid} restarts={restarts} status={status} health={}",
        service.health_topic
    );
    if !detail.is_empty() {
        payload.push_str(&format!(" detail={detail}"));
    }
    broker.publish(&topic, payload.as_bytes(), true);
}

/// Serve queued subscriptions and control calls without blocking.
fn serve_pending(
    services: &mut Vec<Service>,
    broker: &mut router::TopicBroker,
    server: &Endpoint,
    buffer: &mut [u8],
    cache: &mut StatusCache,
) -> messenger::Result<()> {
    while let Some(message) = server.poll_recv_with(buffer)? {
        let interface = message.interface_id();
        let method = message.method();
        let reply = match dispatch(services, broker, &message, cache) {
            Ok(parcel) => parcel,
            // A malformed request still gets an answer, or its caller would
            // wait forever. A structured error is the useful one on the
            // control interface; the topic router keeps an empty reply.
            Err(error) if interface == services::INIT_INTERFACE => {
                services::init_error_reply(method, error)
            }
            Err(_) => Parcel::default(),
        };
        if let Some(txn) = message.txn {
            server.reply_or_drop(txn, &reply)?;
        }
    }
    Ok(())
}

/// Dispatch one inbound message to the broker, the supervision table, or the
/// app registry / launch path.
fn dispatch(
    services: &mut Vec<Service>,
    broker: &mut router::TopicBroker,
    message: &Message,
    cache: &mut StatusCache,
) -> messenger::Result<Parcel> {
    match message.interface_id() {
        router::INTERFACE => broker.handle(message),
        services::INIT_INTERFACE => match message.method() {
            services::init_method::SERVICES => cache.parcel(services),
            services::init_method::LIST_APPS => services::list_apps_reply(&app_infos()),
            services::init_method::LAUNCH => {
                let request = services::decode_launch_request(&message.parcel)?;
                let caller = actor(message)?;
                match launch(services, broker, &request, &caller) {
                    Ok(result) => services::launch_reply(&result),
                    Err(error) => {
                        let target = if request.session == 0 {
                            caller.session
                        } else {
                            request.session
                        };
                        if error.errno() == Some(-messenger::errno::EPERM) {
                            sys::write_str(&format!(
                                "INIT:LAUNCH:DENIED:PASS app={} caller_uid={} caller_session={} target={}\n",
                                request.app, caller.uid, caller.session, target
                            ));
                        } else if error.errno() == Some(-messenger::errno::EAGAIN) {
                            sys::write_str(&format!(
                                "INIT:LAUNCH:CAP:PASS app={} session={} cap={}\n",
                                request.app, target, LAUNCH_CAP_PER_SESSION
                            ));
                        }
                        Err(error)
                    }
                }
            }
            _ => Err(messenger::Error::Errno(-messenger::errno::EINVAL)),
        },
        _ => Err(messenger::Error::Errno(-messenger::errno::EINVAL)),
    }
}

/// The kernel-stamped actor for a message: the supervisor runs as root, so it
/// holds `CAP_SETUID` and may read another task's credential block (the same
/// pattern `clipboardd` uses).
fn actor(message: &Message) -> messenger::Result<SysCred> {
    let mut cred = SysCred::default();
    sys::cred_get(Some(message.sender), &mut cred).map_err(messenger::Error::Errno)?;
    Ok(cred)
}

/// The session-owner policy: a caller may launch into its own session; root or
/// a holder of `CAP_SETUID` (the supervisor) may launch anywhere; everyone
/// else is refused.
fn authorize(caller: &SysCred, target_session: u64) -> messenger::Result<()> {
    if caller.session == target_session || caller.uid == 0 || caller.caps & CAP_SETUID != 0 {
        Ok(())
    } else {
        Err(messenger::Error::Errno(-messenger::errno::EPERM))
    }
}

/// The number of launched rows reserved against [`LAUNCH_CAP_PER_SESSION`]
/// for `session`: `Running` (holding a slot now) plus `Restarting` and
/// `Pending` (will reclaim one without going through [`launch`] again). A
/// `Stopped`/`Failed` row holds nothing and does not count; `launch` already
/// prunes those for the same app before this runs. A launched row's session
/// lives in its stamped credentials (`cred`), since manifest rows (`cred:
/// None`) never count.
fn running_in_session(services: &[Service], session: u64) -> usize {
    services
        .iter()
        .filter(|service| {
            service.launched
                && matches!(
                    service.phase,
                    Phase::Running | Phase::Restarting | Phase::Pending
                )
                && service.cred.map(|cred| cred.session) == Some(session)
        })
        .count()
}

/// The credentials a launched child is stamped with: the target session's
/// uid/gid/session and the session capability set. When the caller launches
/// into its own session, its own uid/gid (and label) apply; root launching into
/// another session resolves the uid/gid from `logind`'s table.
fn target_cred(caller: &SysCred, target_session: u64) -> messenger::Result<SysCred> {
    if caller.session == target_session {
        return Ok(SysCred::new(
            caller.uid,
            caller.gid,
            SESSION_CAPS,
            caller.label_id,
            target_session,
        ));
    }
    let uid = lookup_session_uid(target_session)?;
    Ok(SysCred::new(uid, uid, SESSION_CAPS, 0, target_session))
}

/// The uid of an active `logind` session; `ENOENT` when the service is
/// unreachable or the session is unknown.
fn lookup_session_uid(session: u64) -> messenger::Result<u32> {
    let endpoint = services::resolve_service(logind::NAME)
        .map_err(|_| messenger::Error::Errno(-messenger::errno::ENOENT))?;
    let (_, sessions) = logind::fetch_sessions(&endpoint)
        .map_err(|_| messenger::Error::Errno(-messenger::errno::ENOENT))?;
    sessions
        .iter()
        .find(|record| record.id == session && record.state == "active")
        .map(|record| record.uid)
        .ok_or(messenger::Error::Errno(-messenger::errno::ENOENT))
}

/// Launch an app as a supervised child of this task (issue #158).
///
/// The checks run in order: the app id must be in [`APPS`]; the caller must
/// pass [`authorize`] for the target session; the target session must have
/// fewer than [`LAUNCH_CAP_PER_SESSION`] launched rows reserved (see
/// [`running_in_session`]); the target session's credentials must resolve.
/// The row then spawns
/// immediately with `spawn_as`, and from there the ordinary supervision loop
/// owns it: restart policy, backoff, health topic and service event.
fn launch(
    services: &mut Vec<Service>,
    broker: &mut router::TopicBroker,
    request: &services::LaunchRequest,
    caller: &SysCred,
) -> messenger::Result<services::LaunchResult> {
    let app = find_app(&request.app).ok_or(messenger::Error::Errno(-messenger::errno::ENOENT))?;
    let target_session = if request.session == 0 {
        caller.session
    } else {
        request.session
    };
    authorize(caller, target_session)?;
    if running_in_session(services, target_session) >= LAUNCH_CAP_PER_SESSION {
        return Err(messenger::Error::Errno(-messenger::errno::EAGAIN));
    }
    let cred = target_cred(caller, target_session)?;
    // A stopped or failed launched row for the same app is superseded: the
    // registry keeps the supervision table bounded (manifest rows stay).
    services.retain(|service| {
        !(service.launched
            && service.name == app.id
            && matches!(service.phase, Phase::Stopped | Phase::Failed))
    });
    let mut row = Service::from_app(app, &request.args, cred);
    let command = command_line(&row, 0);
    let Some(pid) = sys::spawn_as(&command, &cred) else {
        sys::write_str(&format!(
            "init: launch {} failed: {} (session {})\n",
            app.id, app.path, target_session
        ));
        return Err(messenger::Error::Errno(-messenger::errno::ENOENT));
    };
    row.pid = pid;
    row.phase = Phase::Running;
    row.started_tick = sys::clock();
    let index = services.len();
    services.push(row);
    sys::write_str(&format!(
        "INIT:LAUNCH:PASS app={} pid={pid} session={target_session}\n",
        app.id
    ));
    publish_state(broker, &services[index], "running", pid, 0, 0, "");
    Ok(services::LaunchResult {
        app: app.id.to_string(),
        pid,
        session: target_session,
    })
}

/// The registry self-test: every row is well formed and the ids `mimed`
/// registers are present. Prints `INIT:APPS:PASS`.
fn selftest_apps() {
    let mut ok = !APPS.is_empty();
    for app in APPS {
        let verbs = app.verbs.len();
        ok &= !app.id.is_empty()
            && !app.name.is_empty()
            && app.path.ends_with(".ELF")
            && (verbs == 0 || verbs <= 4);
    }
    let has_editor = APPS
        .iter()
        .find(|app| app.id == "editor")
        .map(|app| app.verbs.contains(&"open") && app.verbs.contains(&"edit"))
        .unwrap_or(false);
    let has_top = APPS
        .iter()
        .any(|app| app.id == "top" && app.path == "TOP.ELF");
    ok &= has_editor && has_top;
    if ok {
        sys::write_str(&format!("INIT:APPS:PASS count={}\n", APPS.len()));
    } else {
        sys::write_str("INIT:APPS:FAIL registry is malformed\n");
    }
}

/// The launch-policy self-test: a synthetic session owner may launch into its
/// own session, a foreign non-root caller may not, and root may go anywhere.
/// Prints `INIT:LAUNCH:DENIED:PASS` when the denial holds.
fn selftest_launch_policy() {
    let owner = SysCred::new(1000, 1000, 0, 0, 7);
    let foreign = SysCred::new(1000, 1000, 0, 0, 8);
    let root = SysCred::new(0, 0, 0, 0, 3);
    let denied = authorize(&foreign, 7).is_err();
    let owner_ok = authorize(&owner, 7).is_ok();
    let root_ok = authorize(&root, 8).is_ok();
    if denied && owner_ok && root_ok {
        sys::write_str("INIT:LAUNCH:DENIED:PASS\n");
    } else {
        sys::write_str("INIT:LAUNCH:DENIED:FAIL policy check failed\n");
    }
}

/// The launch-cap self-test: a session already holding
/// [`LAUNCH_CAP_PER_SESSION`] reserved launched rows gets `-EAGAIN` for one
/// more, and the refused call leaves the supervision table unchanged (nothing
/// was spawned). One row is `Running` and the other `Restarting` (mid crash
/// backoff), so the test also covers a launched row that no longer holds a
/// task slot but will reclaim one from the supervision loop's backoff sweep
/// without another cap check ([`running_in_session`] must still count it).
/// Exercises the real [`launch`] against a synthetic table, the same way
/// [`selftest_launch_policy`] exercises [`authorize`] directly, so the boot
/// self-test needs no timing-sensitive race against real processes exiting
/// or crashing. Prints `INIT:LAUNCH:CAP:PASS`.
fn selftest_launch_cap() {
    let Some(app) = find_app("top") else {
        return sys::write_str("INIT:LAUNCH:CAP:FAIL top is not registered\n");
    };
    const SESSION: u64 = 4243;
    let cred = SysCred::new(1000, 1000, 0, 0, SESSION);
    let reserved_phases = [Phase::Running, Phase::Restarting];
    let mut services: Vec<Service> = reserved_phases
        .into_iter()
        .cycle()
        .take(LAUNCH_CAP_PER_SESSION)
        .map(|phase| {
            let mut row = Service::from_app(app, "", cred);
            row.phase = phase;
            row
        })
        .collect();
    let mut broker = router::TopicBroker::new("os.lazy.selftest.sink");
    let request = services::LaunchRequest {
        app: String::from("top"),
        args: String::new(),
        session: SESSION,
    };
    let capped = matches!(
        launch(&mut services, &mut broker, &request, &cred),
        Err(messenger::Error::Errno(code)) if code == -messenger::errno::EAGAIN
    );
    if capped && services.len() == LAUNCH_CAP_PER_SESSION {
        sys::write_str(&format!(
            "INIT:LAUNCH:CAP:PASS session={SESSION} cap={LAUNCH_CAP_PER_SESSION}\n"
        ));
    } else {
        sys::write_str("INIT:LAUNCH:CAP:FAIL cap did not hold\n");
    }
}

/// Snapshot the supervision table as wire rows.
fn status_rows(services: &[Service]) -> Vec<services::ServiceStatus> {
    services
        .iter()
        .map(|service| {
            let deps = service.deps.iter().fold(String::new(), |mut text, dep| {
                if !text.is_empty() {
                    text.push(',');
                }
                text.push_str(dep);
                text
            });
            services::ServiceStatus {
                name: service.name.to_string(),
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
