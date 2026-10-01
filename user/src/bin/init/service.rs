//! `init`'s runtime rows: the phase of a supervised service and the record the
//! supervisor keeps for each one, built either from a boot manifest entry, from
//! a built-in app launch, or from an installed app's launch.
//!
//! Split out of `state.rs`, which carries the tuning constants and the boot
//! manifest.

use alloc::string::{String, ToString};

use user::sys::Cred as SysCred;

use super::apps::AppSpec;
use super::installed::InstalledApp;
use super::state::{manifest_cred, Restart, ServiceSpec, BOOT_EVIDENCE};

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
    /// Argument string (manifest default, or the `Launch` request's args).
    pub(super) args: String,
    /// Restart policy applied to exits.
    pub(super) restart: Restart,
    /// Service names that must be `Running` before this row starts.
    pub(super) deps: &'static [&'static str],
    /// Credentials for `spawn_as`; `None` inherits this supervisor's identity
    /// (the manifest path).
    pub(super) cred: Option<SysCred>,
    /// The policy label (`app:<system_name>`) an installed app's child is
    /// stamped with, on every respawn too; `None` for built-in rows.
    pub(super) label: Option<&'static str>,
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
    /// Absolute tick a `Stopping` row must have exited by.
    pub(super) stop_deadline: u64,
    /// Whether the shutdown already sent this row `SIGKILL`.
    pub(super) killed: bool,
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
            cred: manifest_cred(spec.name),
            label: None,
            launched: false,
            linux: false,
            autostart: false,
            phase: Phase::Pending,
            pid: 0,
            restarts: 0,
            started_tick: 0,
            next_start: 0,
            last_status: None,
            stop_deadline: 0,
            killed: false,
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
            label: None,
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
        }
    }

    /// A row for one launch of an installed app: it runs from its install
    /// directory, labelled `app:<system_name>` so the kernel applies the policy
    /// `pkgd` loaded for it. The manifest's fixed arguments come first, then the
    /// request's.
    pub(super) fn from_installed(app: &InstalledApp, args: &str, cred: SysCred) -> Service {
        let mut all_args = app.args.clone();
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
            restart: Restart::OnFailure,
            deps: &[],
            cred: Some(cred),
            label: Some(app.label),
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
        }
    }
}
