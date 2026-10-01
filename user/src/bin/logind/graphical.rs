//! The graphical session path (issue #157): instead of spawning the user's
//! console shell, a graphical login asks `init` to launch the desktop shell
//! (LazyShell, registry id `lazyshell`) into the new session.
//!
//! # Choosing it
//!
//! The confd key `sys/session/mode` (`Str`, written only by root like every
//! `sys/*` key) selects it: `graphical` makes every following login a graphical
//! one; anything else, an absent key or an unreachable `confd` keeps the
//! console path, unchanged. It is read at each login, so
//! `confctl set sys/session/mode graphical` (as root) takes effect at the next
//! prompt without a reboot.
//!
//! # Launching
//!
//! `init`'s `Launch(app, "", session)` does the work: `logind` runs as root, so
//! the session-owner check admits it, and `init` stamps LazyShell with the
//! session's uid and empty session capabilities (`SESSION_CAPS`), supervises
//! it (restart `Always`) and reports its pid. `init` learns the session's uid
//! from the `system/events/login/session/<id>` event published *before* the
//! call (its `sessions.rs`), since it cannot query `logind` while `logind`
//! waits on it. Without the topic bus that event is missing and the launch
//! fails with `ENOENT` after `init`'s query times out.
//!
//! The desktop image's boot-time shell (autostarted by `init` as uid 0 in
//! session 0 until a graphical login exists) is stopped first so two shells
//! never fight over one display; if the session's shell cannot start, the
//! boot-time one is launched again and the login falls back to the console
//! shell. A graphical session lasts until shutdown (there is no logout yet):
//! its shell is `init`'s child, so `logind` cannot wait for it and does not
//! prompt again while it is active.

use alloc::format;
use alloc::string::String;

use user::messenger::{self, confd, logind, router, services};
use user::sys;

/// The confd key that selects the session kind.
pub const SESSION_MODE_KEY: &str = "sys/session/mode";
/// The [`SESSION_MODE_KEY`] value that selects a graphical session.
pub const GRAPHICAL: &str = "graphical";
/// `init`'s registry id for the desktop shell.
const SHELL_APP: &str = "lazyshell";
/// How long a login waits for `confd` or `init` (PIT ticks, 100 Hz).
const CALL_TICKS: u64 = 300;

/// Whether this login should start a graphical session.
pub fn requested() -> bool {
    let Ok(client) = confd::Client::connect() else {
        return false;
    };
    matches!(
        client.with_timeout(CALL_TICKS).get(SESSION_MODE_KEY),
        Ok(Some(::confd::Value::Str(mode))) if mode == GRAPHICAL
    )
}

/// Start the graphical session `session` for `user`/`uid`: announce it, then
/// have `init` launch LazyShell into it. Returns the shell's pid, or `None`
/// (after printing `LOGIN:GRAPHICAL:FAIL`) when the caller should fall back to
/// the console shell.
pub fn start(bus: &mut Option<router::Bus>, user: &str, uid: u32, session: u64) -> Option<u64> {
    // The owner `init` stamps the shell with, announced before the launch.
    publish_session(bus, user, uid, session, 0, "starting");
    let Ok(init) = services::resolve_service(services::INIT_NAME) else {
        return fail(session, "init-unreachable");
    };
    // The boot-time (session 0) shell gives the display up to this session's.
    let replaced = services::stop(&init, SHELL_APP).unwrap_or(0);
    let deadline = Some(sys::clock() + CALL_TICKS);
    match services::launch_by(&init, SHELL_APP, "", session, deadline) {
        Ok(result) => {
            sys::write_str(&format!(
                "LOGIN:GRAPHICAL:PASS user={user} session={session} app={SHELL_APP} pid={}\n",
                result.pid
            ));
            Some(result.pid)
        }
        Err(error) => {
            if replaced > 0 {
                // Put the boot-time shell back so the desktop keeps a taskbar.
                let _ = services::launch_by(&init, SHELL_APP, "", 0, deadline);
            }
            fail(session, &error_text(&error))
        }
    }
}

/// Publish the retained `system/events/login/session/<id>` record.
pub fn publish_session(
    bus: &mut Option<router::Bus>,
    user: &str,
    uid: u32,
    session: u64,
    pid: u64,
    state: &str,
) {
    if let Some(bus) = bus.as_mut() {
        let record = logind::wire::LoginSession {
            user: String::from(user),
            uid,
            pid,
            state: String::from(state),
        };
        let id = format!("{session}");
        let _ = logind::wire::publish_system_events_login_session(bus, &id, &record);
    }
}

/// Report a graphical start that did not happen; the login continues on the
/// console path.
fn fail(session: u64, reason: &str) -> Option<u64> {
    sys::write_str(&format!(
        "LOGIN:GRAPHICAL:FAIL session={session} reason={reason}; starting the console shell\n"
    ));
    None
}

/// A short reason for a failed `Launch`.
fn error_text(error: &messenger::Error) -> String {
    match error {
        messenger::Error::Errno(code) => format!("errno{code}"),
        messenger::Error::Init(code) => format!("init{code}"),
        _ => String::from("launch-failed"),
    }
}
