//! `init`'s runtime rows: the phase of a supervised service and the record the
//! supervisor keeps for each one, built either from a boot manifest entry, from
//! a built-in app launch, or from an installed app's launch.
//!
//! Split out of `state.rs`, which carries the tuning constants and the boot
//! manifest.

use alloc::string::String;
use alloc::vec::Vec;

use user::sys::Cred as SysCred;

use super::apps::AppSpec;
use super::installed::InstalledApp;
use super::lifecycle::Lifecycle;
use super::reload::Hot;
use super::state::{manifest_cred, Restart, ServiceSpec, BOOT_EVIDENCE, LINUX_ROWS};

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
    /// Asked to stop by an orderly shutdown; its exit is due by
    /// `stop_deadline`, after which it is killed.
    Stopping,
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
            Phase::Stopping => "stopping",
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
    /// Arguments after `argv[0]` (the manifest's, or the app's fixed ones
    /// then the `Launch` request's path), one `argv` item each.
    pub(super) args: Vec<String>,
    /// Restart policy applied to exits.
    pub(super) restart: Restart,
    /// Service names that must be `Running` before this row starts.
    pub(super) deps: &'static [&'static str],
    /// Credentials the child is stamped with; `None` inherits this supervisor's identity
    /// (the manifest path).
    pub(super) cred: Option<SysCred>,
    /// The policy label (`app:<system_name>`) an installed app's child is
    /// stamped with, on every respawn too; `None` for built-in rows.
    pub(super) label: Option<&'static str>,
    /// The child's environment, on every respawn too: a launched app's
    /// session `HOME`, `USER` and `PATH` (`sessions::env`, issue #508); empty
    /// for a manifest service.
    pub(super) env: Vec<String>,
    /// Whether the row came from `Launch` (vs the boot manifest).
    pub(super) launched: bool,
    /// Whether the program is a Linux-ABI binary (the Linux personality).
    pub(super) linux: bool,
    /// Whether `init` itself opened the row at boot (the desktop's apps); it
    /// does not count against the session's launch cap.
    pub(super) autostart: bool,
    pub(super) phase: Phase,
    /// Task slot of the running child; `0` between runs.
    pub(super) pid: u64,
    /// Rapid-crash counter (reset once a run is `svcpolicy::STABLE_TICKS` old).
    pub(super) restarts: u64,
    /// Tick the current run started at.
    pub(super) started_tick: u64,
    /// Absolute tick the next restart is due at.
    pub(super) next_start: u64,
    /// Exit status of the last run.
    pub(super) last_status: Option<u64>,
    /// Absolute tick a `Stopping` row must have exited by.
    pub(super) stop_deadline: u64,
    /// Whether the shutdown already sent this row `SIGKILL`.
    pub(super) killed: bool,
    /// Whether the current run said it serves (`init.Ready`, `ready.rs`).
    pub(super) ready: bool,
    /// The display name a failure notice shows (the app's, or the row name).
    pub(super) title: String,
    /// Why the current run says it is failing (`init.ReportFailure`, already
    /// cleaned); cleared at every spawn.
    pub(super) reason: Option<String>,
    /// A resident app's lifecycle (`os.lazy.init.app.v1`, docs/tray-plan.md
    /// section 5): its event channel, queued `Reopen`s, a requested quit.
    pub(super) life: Lifecycle,
    /// The `Launch` request's path argument of a launched row, so an app
    /// swap (`relaunch.rs`) starts the new build on the same document.
    pub(super) launch_arg: Option<String>,
    /// A hot reload's binary, trial and outcome (`reload.rs`).
    pub(super) hot: Hot,
}

impl Service {
    /// A row for one manifest entry (spawned with this task's identity).
    pub(super) fn from_manifest(spec: &'static ServiceSpec) -> Service {
        Service {
            name: spec.name,
            path: spec.path,
            // `soak=`/`demo=` only drive boot evidence; release and desktop
            // boots skip them. The manifest writes them as one word list.
            args: if BOOT_EVIDENCE {
                spec.args.split_whitespace().map(String::from).collect()
            } else {
                Vec::new()
            },
            restart: spec.restart,
            deps: spec.deps,
            cred: manifest_cred(spec.name),
            label: None,
            env: Vec::new(),
            launched: false,
            linux: LINUX_ROWS.contains(&spec.name),
            autostart: false,
            phase: Phase::Pending,
            pid: 0,
            restarts: 0,
            started_tick: 0,
            next_start: 0,
            last_status: None,
            stop_deadline: 0,
            killed: false,
            ready: false,
            title: String::from(spec.name),
            reason: None,
            life: Lifecycle::default(),
            hot: Hot::default(),
            launch_arg: None,
        }
    }

    /// A row for one app launch, stamped with the target session's credentials.
    /// The registry's default arguments come first, then the request's path.
    pub(super) fn from_app(app: &'static AppSpec, path: Option<String>, cred: SysCred) -> Service {
        let all_args = app
            .args
            .iter()
            .map(|arg| String::from(*arg))
            .chain(path.clone())
            .collect();
        Service {
            name: app.id,
            path: app.path,
            args: all_args,
            restart: app.restart,
            deps: &[],
            cred: Some(cred),
            label: None,
            env: Vec::new(),
            launched: true,
            linux: app.linux,
            autostart: false,
            phase: Phase::Pending,
            pid: 0,
            restarts: 0,
            started_tick: 0,
            next_start: 0,
            last_status: None,
            stop_deadline: 0,
            killed: false,
            ready: false,
            title: String::from(app.name),
            reason: None,
            life: Lifecycle::default(),
            hot: Hot::default(),
            launch_arg: path,
        }
    }

    /// A row for one launch of an installed app: it runs from its install
    /// directory, labelled `app:<system_name>` so the kernel applies the policy
    /// `pkgd` loaded for it. The manifest's fixed arguments come first, as the
    /// manifest lists them, then the request's path.
    pub(super) fn from_installed(
        app: &InstalledApp,
        path: Option<String>,
        cred: SysCred,
    ) -> Service {
        let all_args = app.args.iter().cloned().chain(path.clone()).collect();
        Service {
            name: app.id,
            path: app.path,
            args: all_args,
            restart: app.restart,
            deps: &[],
            cred: Some(cred),
            label: Some(app.label),
            env: Vec::new(),
            launched: true,
            linux: app.linux,
            autostart: false,
            phase: Phase::Pending,
            pid: 0,
            restarts: 0,
            started_tick: 0,
            next_start: 0,
            last_status: None,
            stop_deadline: 0,
            killed: false,
            ready: false,
            title: app.name.clone(),
            reason: None,
            life: Lifecycle::resident(app.resident),
            hot: Hot::default(),
            launch_arg: path,
        }
    }
}
