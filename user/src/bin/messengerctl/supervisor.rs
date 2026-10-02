//! Supervisor, app-registry and launch commands, plus the app self-test.

use alloc::format;
use alloc::string::String;
use user::messenger::{self, services};
use user::sys;

use super::commands::report;
use super::selftest::park_tick;

/// `services`: the supervisor's supervision table (issue #93).
pub(crate) fn print_services() {
    let endpoint = match services::resolve_service(services::INIT_NAME) {
        Ok(endpoint) => endpoint,
        Err(error) => return report(error.message()),
    };
    match services::fetch_services(&endpoint) {
        Ok(statuses) if statuses.is_empty() => {
            sys::write_str("services: no services supervised\n");
        }
        Ok(statuses) => {
            sys::write_str(&format!("services: {} supervised\n", statuses.len()));
            for status in &statuses {
                sys::write_str(&format!(
                    "  {:<10} {:<10} pid {:<3} restarts {} health {}\n",
                    status.name, status.state, status.pid, status.restarts, status.health
                ));
                if !status.deps.is_empty() {
                    sys::write_str(&format!("    deps {}\n", status.deps));
                }
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `apps`: the supervisor's built-in app registry (issue #158), the table the
/// S5 start menu enumerates and `launch` resolves against.
pub(crate) fn print_apps() {
    let endpoint = match services::resolve_service(services::INIT_NAME) {
        Ok(endpoint) => endpoint,
        Err(error) => return report(error.message()),
    };
    match services::fetch_apps(&endpoint) {
        Ok(apps) if apps.is_empty() => {
            sys::write_str("apps: registry is empty\n");
        }
        Ok(apps) => {
            sys::write_str(&format!("apps: {} registered\n", apps.len()));
            for app in &apps {
                let verbs = if app.verbs.is_empty() {
                    String::from("-")
                } else {
                    app.verbs.join(",")
                };
                sys::write_str(&format!(
                    "  {:<14} {:<20} {:<12} {:<10} {}\n",
                    app.id, app.name, app.path, app.restart, verbs
                ));
            }
        }
        Err(error) => report(error.message()),
    }
}

/// `launch <app> [args]`: ask `init` to spawn the app in this task's session
/// (issue #158) and print the child's pid. The app's own output and exit are
/// the evidence that it ran.
pub(crate) fn launch_app(rest: &str) {
    let rest = rest.trim();
    let (app, args) = match rest.find(char::is_whitespace) {
        Some(index) => (&rest[..index], rest[index..].trim()),
        None => (rest, ""),
    };
    if app.is_empty() {
        return report("usage: launch <app> [args]");
    }
    match services::launch_app(app, args, 0) {
        Ok(result) => sys::write_str(&format!(
            "launch {} -> pid {} (session {})\n",
            result.app, result.pid, result.session
        )),
        Err(error) => report(error.message()),
    }
}

/// The boot-time app-registry and launch path self-test (issue #158). Silent
/// when `init` is not reachable, like [`topic_selftest`]; otherwise it:
///
/// 1. lists the registry and checks the ids `mimed` registers are present;
/// 2. launches `top` into this task's (session 0) session and prints
///    `MSGCTL:LAUNCH:PASS` (`init` prints its own `INIT:LAUNCH:PASS`, and the
///    app prints `SYS:TOP:PASS`);
/// 3. spawns a `/system/bin/messengerctl probe` child ([`probe_role`]) that restamps
///    *itself* into a foreign session and asks `init` to launch into this
///    console's original session: the supervisor must refuse with `-EPERM`
///    (`INIT:LAUNCH:DENIED:PASS`, `MSGCTL:LAUNCH:DENIED:PASS`). Running the
///    probe in a child, not this task, matters: the kernel never lets a task
///    widen its own credentials back up, so a probe that dropped this
///    console's own privilege could never restore it, and `commands()` would
///    serve the rest of the session as the probe's uid.
pub(crate) fn app_selftest() {
    let Some(endpoint) = resolve_init() else {
        return;
    };
    match services::fetch_apps(&endpoint) {
        Ok(apps) => {
            // `ListApps` returns only the apps this image ships, so assert the
            // always-shipped rows (`top`, and this tool) with the path/verb the
            // registry declares. `editor` and the other manifest rows are
            // listed only when an image ships their ELF, so they are not
            // required here.
            let has_top = apps.iter().any(|app| {
                app.id == "top"
                    && app.path == fhs::bin::TOP
                    && app.verbs.iter().any(|v| v == "open")
            });
            let has_self = apps
                .iter()
                .any(|app| app.id == "messengerctl" && app.path == fhs::bin::MESSENGERCTL);
            if has_top && has_self {
                sys::write_str(&format!("MSGCTL:APPS:PASS count={}\n", apps.len()));
            } else {
                sys::write_str("MSGCTL:APPS:FAIL registry is missing top/messengerctl\n");
            }
        }
        Err(error) => sys::write_str(&format!("MSGCTL:APPS:FAIL:{}\n", error.message())),
    }

    match services::launch(&endpoint, "top", "", 0) {
        Ok(result) => sys::write_str(&format!(
            "MSGCTL:LAUNCH:PASS app={} pid={}\n",
            result.app, result.pid
        )),
        Err(error) => sys::write_str(&format!("MSGCTL:LAUNCH:FAIL:{}\n", error.message())),
    }

    // The foreign-session probe runs in a short-lived child (see
    // [`probe_role`]): a task that has dropped to uid 1000 cannot regain
    // root, so this console task must not be the one that self-transitions.
    match sys::spawn(&user::cmdline::native(fhs::bin::MESSENGERCTL, "probe")) {
        Some(pid) => reap_probe(pid),
        None => sys::write_str("MSGCTL:LAUNCH:DENIED:FAIL could not spawn the probe\n"),
    }
}

/// The `/system/bin/messengerctl probe` child: become uid 1000 in session 4242 (a
/// self-transition the kernel audits; the dropped capability set means the
/// launch call can no longer pass the supervisor's privilege check), try to
/// launch into the console's original session (0), print the
/// `MSGCTL:LAUNCH:DENIED:*` evidence line, and exit. This never runs in the
/// console task itself: see [`app_selftest`].
pub(crate) fn probe_role() -> ! {
    let endpoint = match resolve_init() {
        Some(endpoint) => endpoint,
        None => {
            sys::write_str("MSGCTL:LAUNCH:DENIED:FAIL could not resolve init\n");
            sys::exit(1);
        }
    };
    let probe = sys::Cred::new(1000, 1000, 0, 0, 4242);
    if sys::cred_set(None, &probe).is_err() {
        sys::write_str("MSGCTL:LAUNCH:DENIED:FAIL could not enter the probe session\n");
        sys::exit(1);
    }
    match services::launch(&endpoint, "top", "", 1) {
        Err(error) if error.errno() == Some(-messenger::errno::EPERM) => {
            sys::write_str("MSGCTL:LAUNCH:DENIED:PASS\n");
            sys::exit(0);
        }
        Ok(_) => {
            sys::write_str("MSGCTL:LAUNCH:DENIED:FAIL foreign launch was allowed\n");
            sys::exit(1);
        }
        Err(error) => {
            sys::write_str(&format!("MSGCTL:LAUNCH:DENIED:FAIL:{}\n", error.message()));
            sys::exit(1);
        }
    }
}

/// Reap the probe child, bounded so a spawn that never runs cannot hang the
/// console forever; other exits (there should be none yet) are reaped and
/// ignored until the probe's own pid turns up.
fn reap_probe(pid: u64) {
    const ATTEMPTS: usize = 200;
    for _ in 0..ATTEMPTS {
        match sys::wait(sys::clock() + 5) {
            Some((exited, _status)) if exited == pid => return,
            Some(_) => {}
            None => {}
        }
    }
    sys::write_str("MSGCTL:LAUNCH:DENIED:FAIL probe did not exit\n");
}

/// Resolve `init`, retrying while the supervisor's registration lands; `None`
/// when it never does.
fn resolve_init() -> Option<user::messenger::Endpoint> {
    const ATTEMPTS: usize = 64;
    for _ in 0..ATTEMPTS {
        if let Ok(endpoint) = services::resolve_service(services::INIT_NAME) {
            return Some(endpoint);
        }
        park_tick();
    }
    None
}
