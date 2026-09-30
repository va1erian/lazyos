//! `init`'s supervision model: the tuning constants that shape restart/backoff
//! and session-launch policy, the boot manifest, the runtime service rows the
//! supervisor tracks, and the restart policy shared with the app registry.
//!
//! Split out of `init.rs` (issue #194); a pure move, no behavior change.

use alloc::string::{String, ToString};

use user::sys::Cred as SysCred;

use super::apps::AppSpec;

/// Serve subscriptions and due restarts at least this often (PIT ticks).
pub(super) const POLL_TICKS: u64 = 5;
/// First restart delay (PIT ticks, 100 Hz), doubled per rapid crash.
pub(super) const BACKOFF_BASE: u64 = 10;
/// Restart delay cap, so a crash loop stays gentle.
pub(super) const BACKOFF_MAX: u64 = 300;
/// A service that stayed up this long is considered recovered: its restart
/// counter resets, so occasional crashes never exhaust the budget.
pub(super) const STABLE_TICKS: u64 = 100;
/// Give up restarting a service after this many *rapid* crashes.
pub(super) const MAX_RESTARTS: u64 = 5;
/// Capabilities a launched session child receives. Empty today, matching
/// `logind`'s session set: least privilege is the default and the S5.2
/// session grants arrive through the credential gate.
pub(super) const SESSION_CAPS: u32 = 0;
/// `CAP_SETUID` (kernel `ipc::credentials`): a supervisor holding it may
/// launch into any session.
pub(super) const CAP_SETUID: u32 = 1 << 6;
/// Ticks after boot before the launch self-test first tries (100 Hz). The
/// manifest's `Once` services (top) exit around here, freeing their slots.
pub(super) const LAUNCH_SELFTEST_DELAY: u64 = 30;
/// Ticks between launch self-test attempts while no slot is free.
pub(super) const LAUNCH_SELFTEST_RETRY: u64 = 25;
/// Give up on the launch self-test after this many attempts.
pub(super) const LAUNCH_SELFTEST_ATTEMPTS: u64 = 40;
/// Ticks after boot before the first autostart app opens (`xuid` is up by
/// then; an app that is early just waits in `Client::connect`).
pub(super) const AUTOSTART_DELAY: u64 = 50;
/// Ticks between two autostart launches.
pub(super) const AUTOSTART_STAGGER: u64 = 40;
/// Autostart retries per app while the task table is full.
pub(super) const AUTOSTART_ATTEMPTS: u64 = 40;
/// Launched rows one session may hold reserved at once (issue #177). The
/// boot manifest's own services already run the task table
/// (`kernel/src/task/mod.rs`'s `MAX_TASKS`) close to full for the life of the
/// boot, so the cap is a small fixed number rather than derived from the
/// live table: it must hold room for supervised restarts and new logins even
/// when nothing else has freed a slot yet. A row still reserves its slot
/// while `Restarting`: [`spawn_service`] respawns it from the main loop's
/// backoff sweep, not through [`launch`], so a crashed row that stopped
/// counting here could let a session accumulate more rows than the cap once
/// they all came back up. [`running_in_session`] counts every phase that
/// currently holds or will reclaim a slot without another cap check.
pub(super) const LAUNCH_CAP_PER_SESSION: usize = 2;

/// Whether this is the desktop profile (`LAZYOS_DESKTOP=1`, issue #217): the
/// image is a user-facing session, not an evidence boot. `init` keeps the
/// demo-only programs out of it — the deliberate-crash service, the clipboard
/// demo pair and the `top` launch self-test — so a desktop log shows only the
/// real services and apps.
const DESKTOP: bool = cfg!(lazyos_desktop);

/// Whether this boot runs its self-tests (soak, demo clients, launch checks).
/// They are evidence for headless/CI runs and cost boot time, so optimized
/// release builds leave them out, as does the desktop profile (a desktop boot
/// starts only the real session, never the evidence programs).
pub(super) const BOOT_SELFTESTS: bool = cfg!(debug_assertions);

/// Whether this boot starts the evidence-only *programs* (the `soak=`/`demo=`
/// clients, the `flaky` crash service and the `top` launch self-test). Kept
/// separate from [`BOOT_SELFTESTS`] because the desktop profile still serves
/// the registry and policy self-tests but must not start demo programs.
pub(super) const BOOT_EVIDENCE: bool = BOOT_SELFTESTS && !DESKTOP;

/// What to do when a service exits.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Restart {
    /// Restart on any exit.
    Always,
    /// Restart only when the exit status is non-zero.
    OnFailure,
    /// Never restart.
    Once,
}

impl Restart {
    /// The wire word `ListApps` reports.
    pub(super) fn label(self) -> &'static str {
        match self {
            Restart::Always => "always",
            Restart::OnFailure => "on-failure",
            Restart::Once => "once",
        }
    }
}

/// One manifest row: the fields the supervisor needs to start and watch a
/// service. The service's retained health topic is derived from its name via
/// the generated `system/health/{name}` helper, so it is not stored here.
pub(super) struct ServiceSpec {
    pub(super) name: &'static str,
    path: &'static str,
    args: &'static str,
    restart: Restart,
    deps: &'static [&'static str],
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
pub(super) const MANIFEST: &[ServiceSpec] = &[
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
    },
    ServiceSpec {
        name: "keyd",
        path: "KEYD.ELF",
        args: "",
        restart: Restart::Always,
        deps: &["messengerd"],
    },
    // The configuration registry (issue #260). It needs `messengerd` for the
    // change-topic broker; the store lives on the kernel VFS, so no service
    // dependency. Started before config consumers; `demo=1` spawns one
    // `confctl` self-test that proves the set/get/list/delete path over the
    // real Messenger transport and prints `CONFCTL:SELFTEST:PASS`.
    ServiceSpec {
        name: "confd",
        path: "CONFD.ELF",
        args: "demo=1",
        restart: Restart::Always,
        deps: &["messengerd"],
    },
    // The time-of-day service (issue #369): UTC from the kernel wall clock,
    // the zone from `confd` (`sys/time/zone`), and the retained `time/tick`
    // topic on the broker, so it needs both. `demo=1` drives a zone change
    // through `confd` and prints `TIMED:DEMO:PASS` once the service followed.
    ServiceSpec {
        name: "timed",
        path: "TIMED.ELF",
        args: "demo=1",
        restart: Restart::Always,
        deps: &["messengerd", "confd"],
    },
    ServiceSpec {
        name: "accountsd",
        path: "ACCTD.ELF",
        args: "",
        restart: Restart::Always,
        deps: &[],
    },
    ServiceSpec {
        name: "logind",
        path: "LOGIND.ELF",
        args: "",
        restart: Restart::Always,
        deps: &["accountsd"],
    },
    ServiceSpec {
        name: "logd",
        path: "LOGD.ELF",
        args: "",
        restart: Restart::Always,
        deps: &["messengerd"],
    },
    ServiceSpec {
        name: "healthd",
        path: "HEALTHD.ELF",
        args: "",
        restart: Restart::Always,
        deps: &["messengerd"],
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
    },
    ServiceSpec {
        name: "flaky",
        path: "FLAKY.ELF",
        args: "",
        restart: Restart::OnFailure,
        deps: &["healthd"],
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
    },
];

/// Runtime phase of a service; `label` is the word published in events and
/// shown by `messengerctl services`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
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
    pub(super) fn label(self) -> &'static str {
        match self {
            Phase::Pending => "pending",
            Phase::Running => "running",
            Phase::Restarting => "restarting",
            Phase::Stopped => "stopped",
            Phase::Failed => "failed",
        }
    }

    /// The health word this phase implies for the `Services` table.
    pub(super) fn health(self) -> &'static str {
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
pub(super) struct Service {
    /// Name published in events and shown by `messengerctl services`.
    pub(super) name: &'static str,
    /// On-disk ELF path.
    pub(super) path: &'static str,
    /// Argument string (manifest default, or the `Launch` request's args).
    pub(super) args: String,
    /// Restart policy applied to exits.
    pub(super) restart: Restart,
    /// Service names that must be `Running` before this row starts.
    pub(super) deps: &'static [&'static str],
    /// Credentials for `spawn_as`; `None` inherits this supervisor's identity
    /// (the manifest path).
    pub(super) cred: Option<SysCred>,
    /// Whether the row came from `Launch` (vs the boot manifest).
    pub(super) launched: bool,
    /// Whether the program is a Linux-ABI binary (spawned with `linux:`).
    pub(super) linux: bool,
    /// Whether `init` itself opened the row at boot (the desktop's apps); it
    /// does not count against the session's launch cap.
    pub(super) autostart: bool,
    pub(super) phase: Phase,
    /// Task slot of the running child; `0` between runs.
    pub(super) pid: u64,
    /// Rapid-crash counter (reset once a run is [`STABLE_TICKS`] old).
    pub(super) restarts: u64,
    /// Tick the current run started at.
    pub(super) started_tick: u64,
    /// Absolute tick the next restart is due at.
    pub(super) next_start: u64,
    /// Exit status of the last run.
    pub(super) last_status: Option<u64>,
}

impl Service {
    /// A row for one manifest entry (spawned with this task's identity).
    pub(super) fn from_manifest(spec: &'static ServiceSpec) -> Service {
        Service {
            name: spec.name,
            path: spec.path,
            // `soak=`/`demo=` only drive boot evidence; release and desktop
            // boots skip them.
            args: if BOOT_EVIDENCE {
                spec.args.to_string()
            } else {
                String::new()
            },
            restart: spec.restart,
            deps: spec.deps,
            cred: None,
            launched: false,
            linux: false,
            autostart: false,
            phase: Phase::Pending,
            pid: 0,
            restarts: 0,
            started_tick: 0,
            next_start: 0,
            last_status: None,
        }
    }

    /// A row for one app launch, stamped with the target session's credentials.
    /// The registry's default arguments come first, then the request's.
    pub(super) fn from_app(app: &'static AppSpec, args: &str, cred: SysCred) -> Service {
        let mut all_args = String::from(app.args);
        if !args.is_empty() {
            if !all_args.is_empty() {
                all_args.push(' ');
            }
            all_args.push_str(args);
        }
        Service {
            name: app.id,
            path: app.path,
            args: all_args,
            restart: app.restart,
            deps: &[],
            cred: Some(cred),
            launched: true,
            linux: app.linux,
            autostart: false,
            phase: Phase::Pending,
            pid: 0,
            restarts: 0,
            started_tick: 0,
            next_start: 0,
            last_status: None,
        }
    }
}
