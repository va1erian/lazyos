//! `init`'s app-launch path: caller authorization, session credential
//! resolution and the launch-cap check, then spawning and adopting the app as
//! a supervised child.
//!
//! Split out of `init.rs` (issue #194); a pure move, no behavior change.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use user::messenger::{self, logind, router, services, Message};
use user::sys::{self, Cred as SysCred};

use super::apps::{find_app, is_available};
use super::installed::{report_label, InstalledApp, InstalledApps};
use super::sessions;
use super::state::{Phase, Service, CAP_SETUID, LAUNCH_CAP_PER_SESSION, SESSION_CAPS};
use super::supervise::{publish_state, spawn_row};

/// The kernel-stamped actor for a message: the credentials its sender had
/// when it was queued (issue #446).
pub(super) fn actor(message: &Message) -> messenger::Result<SysCred> {
    Ok(message.caller())
}

/// The session-owner policy: a caller may launch into its own session; root or
/// a holder of `CAP_SETUID` (the supervisor) may launch anywhere; everyone
/// else is refused.
pub(super) fn authorize(caller: &SysCred, target_session: u64) -> messenger::Result<()> {
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
                && !service.autostart
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
/// unreachable or the session is unknown. A session `logind` announced on the
/// broker is answered from [`sessions::owner`] without a call, which is what
/// lets `logind` itself launch into a session it just created.
fn lookup_session_uid(session: u64) -> messenger::Result<u32> {
    if let Some(uid) = sessions::owner(session) {
        return Ok(uid);
    }
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

/// The most bytes of a launch path argument (the kernel's `argv` block is
/// bounded at 4096 bytes; the program path and fixed args need room).
pub(super) const MAX_LAUNCH_PATH: usize = 1024;

/// Validate the request's `args`: empty (no argument), one absolute path
/// (starting with `/`) or one URL (`scheme:...`, a scheme being a letter then
/// letters, digits, `+`, `-` or `.`), at most [`MAX_LAUNCH_PATH`] bytes, with
/// no NUL or other control character. A URL is how `mimed` hands a link to
/// the app registered for its scheme (`x-scheme-handler/https`). The argument
/// becomes exactly one `argv` item after the app's fixed arguments (`spawnv`
/// splits nothing), so spaces and quotes are kept and there is no way to
/// smuggle a second argument.
pub(super) fn launch_argument(args: &str) -> messenger::Result<Option<String>> {
    if args.is_empty() {
        return Ok(None);
    }
    let valid = args.len() <= MAX_LAUNCH_PATH
        && (args.starts_with('/') || has_url_scheme(args))
        && !args.chars().any(char::is_control);
    if !valid {
        return Err(messenger::Error::Errno(-messenger::errno::EINVAL));
    }
    Ok(Some(args.to_string()))
}

/// Whether `text` starts with a URL scheme and its `:`. A scheme starts with
/// a letter, so a URL can never read as a `-flag`.
fn has_url_scheme(text: &str) -> bool {
    let Some((scheme, _)) = text.split_once(':') else {
        return false;
    };
    let mut bytes = scheme.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

/// Launch an app as a supervised child of this task (issue #158).
///
/// The app id is a built-in registry row ([`find_app`]) or, failing that, an
/// app the package manager installed ([`InstalledApps`], re-read from `confd`
/// here so an install a moment ago is launchable at once): since F5 every
/// desktop app is one, and a core app also answers to its bare short id.
/// Either way the same checks run in the same order ([`admit`]): the caller must pass [`authorize`]
/// for the target session; the argument must be one absolute path; the target
/// session must have fewer than [`LAUNCH_CAP_PER_SESSION`] launched rows
/// reserved (see [`running_in_session`]); the target session's credentials must
/// resolve. The row then spawns immediately, and from there the ordinary
/// supervision loop owns it: restart policy, backoff, health topic and service
/// event. An installed app is spawned stamped with its label (`app:<system
/// name>`), so the kernel applies the policy `pkgd` loaded for it from its first
/// instruction. Both kinds start with the session's environment (`HOME`,
/// `USER`, `PATH`; [`sessions::env`]).
pub(super) fn launch(
    services: &mut Vec<Service>,
    broker: &mut router::TopicBroker,
    installed: &mut InstalledApps,
    request: &services::LaunchRequest,
    caller: &SysCred,
    autostart: bool,
) -> messenger::Result<services::LaunchResult> {
    if find_app(&request.app).is_none() {
        installed.refresh();
        if let Some(app) = installed.find(&request.app) {
            return launch_installed(services, broker, app, request, caller, autostart);
        }
    }
    launch_row(services, broker, request, caller, autostart)
}

/// The checks every launch passes before anything spawns: the session policy,
/// the argument, the per-session cap (unless `init` itself opens the app) and
/// the credentials to stamp. Returns `(path argument, credentials, session)`.
fn admit(
    services: &[Service],
    request: &services::LaunchRequest,
    caller: &SysCred,
    autostart: bool,
) -> messenger::Result<(Option<String>, SysCred, u64)> {
    let target_session = if request.session == 0 {
        caller.session
    } else {
        request.session
    };
    authorize(caller, target_session)?;
    let path_arg = launch_argument(&request.args)?;
    if !autostart && running_in_session(services, target_session) >= LAUNCH_CAP_PER_SESSION {
        return Err(messenger::Error::Errno(-messenger::errno::EAGAIN));
    }
    let cred = target_cred(caller, target_session)?;
    Ok((path_arg, cred, target_session))
}

/// [`launch`], for either a client request or `init`'s own autostart. An
/// `autostart` row is opened by the supervisor itself, so it is exempt from
/// the per-session cap and does not count against it afterwards. An app whose
/// ELF this image does not ship is refused with `-ENOENT` *before* anything
/// is spawned or logged: the registry lists it, the image just lacks it.
pub(super) fn launch_row(
    services: &mut Vec<Service>,
    broker: &mut router::TopicBroker,
    request: &services::LaunchRequest,
    caller: &SysCred,
    autostart: bool,
) -> messenger::Result<services::LaunchResult> {
    let app = find_app(&request.app).ok_or(messenger::Error::Errno(-messenger::errno::ENOENT))?;
    if !is_available(app) {
        return Err(messenger::Error::Errno(-messenger::errno::ENOENT));
    }
    let (path_arg, cred, session) = admit(services, request, caller, autostart)?;
    retire_stopped(services, app.id);
    let mut row = Service::from_app(app, path_arg, cred);
    row.env = sessions::env(session, cred.uid);
    start_row(services, broker, row, &cred, session, autostart)
}

/// Launch one installed app: [`launch_row`]'s path with the installed row, its
/// label and its own program. The app id may be a core app's short alias
/// (`editor`); the row is always named by its `system_name`.
fn launch_installed(
    services: &mut Vec<Service>,
    broker: &mut router::TopicBroker,
    app: &InstalledApp,
    request: &services::LaunchRequest,
    caller: &SysCred,
    autostart: bool,
) -> messenger::Result<services::LaunchResult> {
    if app.resident {
        if let Some(result) = reopen_existing(services, app.id, request, caller)? {
            return Ok(result);
        }
    }
    let (path_arg, cred, session) = admit(services, request, caller, autostart)?;
    retire_stopped(services, app.id);
    let mut row = Service::from_installed(app, path_arg, cred);
    row.env = sessions::env(session, cred.uid);
    let result = start_row(services, broker, row, &cred, session, autostart)?;
    report_label(result.pid, app.id);
    Ok(result)
}

/// A resident app runs once per session (docs/tray-plan.md section 5): a
/// launch while an instance runs in the target session starts nothing, its
/// argument reaches the instance as `Reopen` (queued until it watches), and
/// the answer is that instance with `existing` set. A quitting instance
/// (`Stopping`) does not count: the new launch starts a fresh one.
fn reopen_existing(
    services: &mut [Service],
    id: &str,
    request: &services::LaunchRequest,
    caller: &SysCred,
) -> messenger::Result<Option<services::LaunchResult>> {
    let session = if request.session == 0 {
        caller.session
    } else {
        request.session
    };
    authorize(caller, session)?;
    let args = launch_argument(&request.args)?.unwrap_or_default();
    let Some(row) = services.iter_mut().find(|row| {
        row.launched
            && row.name == id
            && row.phase == Phase::Running
            && row.cred.map(|cred| cred.session) == Some(session)
    }) else {
        return Ok(None);
    };
    super::lifecycle::reopen(row, &args);
    let pid = row.pid;
    sys::write_str(&format!(
        "INIT:LAUNCH:EXISTING app={id} pid={pid} session={session}\n"
    ));
    Ok(Some(services::LaunchResult {
        app: id.to_string(),
        pid,
        session,
        existing: true,
    }))
}

/// A stopped or failed launched row for the same app is superseded: the
/// registry keeps the supervision table bounded (manifest rows stay).
fn retire_stopped(services: &mut Vec<Service>, id: &str) {
    services.retain(|service| {
        !(service.launched
            && service.name == id
            && matches!(service.phase, Phase::Stopped | Phase::Failed))
    });
}

/// Spawn `row` stamped with `cred` (and its label, for an installed app) and
/// adopt it as a supervised child.
fn start_row(
    services: &mut Vec<Service>,
    broker: &mut router::TopicBroker,
    mut row: Service,
    cred: &SysCred,
    session: u64,
    autostart: bool,
) -> messenger::Result<services::LaunchResult> {
    row.autostart = autostart;
    let Some(pid) = spawn_row(&row, 0, Some(*cred)) else {
        sys::write_str(&format!(
            "init: launch {} failed: {} (session {})\n",
            row.name, row.path, session
        ));
        return Err(messenger::Error::Errno(-messenger::errno::ENOENT));
    };
    row.pid = pid;
    row.phase = Phase::Running;
    row.started_tick = sys::clock();
    let id = row.name;
    let index = services.len();
    services.push(row);
    sys::write_str(&format!(
        "INIT:LAUNCH:PASS app={id} pid={pid} session={session}\n"
    ));
    publish_state(broker, &services[index], "running", pid, 0, 0, "");
    Ok(services::LaunchResult {
        app: id.to_string(),
        pid,
        session,
        existing: false,
    })
}
