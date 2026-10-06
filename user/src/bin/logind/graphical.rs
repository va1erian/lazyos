//! The graphical session (issues #157, #623): on a desktop image `logind`
//! does not prompt on the console. It keeps one desktop session at a time and
//! a login screen whenever there is none:
//!
//! * **at boot** it logs the build's autologin account straight in
//!   (`LAZYOS_AUTOLOGIN`, for development and the screenshot sessions) or has
//!   `init` start the login screen (`greeter`, `xui-app/src/bin/greeter.rs`),
//!   which runs as the `_greeter` system uid and calls `Login(name, secret)`;
//! * **a login** (typed or automatic, the same [`Desktop::open`] path) mints
//!   the session id, announces the owner on
//!   `system/events/login/session/<id>` (so `init` can stamp the session's
//!   programs without asking back), and has `init` launch LazyShell into it.
//!   `init` stamps the shell, and the autostart apps that follow it, with the
//!   user's uid and gid, the session id and **no capability**: nothing in a
//!   desktop session runs as root. The login screen is then stopped;
//! * **`Logout()`**, from any task of the session (LazyShell's Log out
//!   button), publishes `system/events/login/end`: `init` ends every task of
//!   the session (`user/src/bin/init/logout.rs`), `xuid` forgets which
//!   session owned the display, and the login screen comes back.
//!
//! # Choosing it
//!
//! The confd key `sys/session/mode` (`Str`) selects the kind of login:
//! `graphical` or `console`. Without the key a desktop image
//! (`LAZYOS_DESKTOP=1`) is graphical and every other image keeps the console
//! prompt; a desktop image whose login screen cannot start (built with
//! `LAZYOS_SHELL=0`) falls back to the console too (`LOGIN:GRAPHICAL:FAIL`).
//!
//! Serial: `LOGIN:GREETER:PASS pid=<p>`, `LOGIN:AUTOLOGIN:PASS user=<u>` or
//! `LOGIN:AUTOLOGIN:FAIL user=<u> reason=<r>`, then the console path's
//! `LOGIN:OK:PASS` / `LOGIN:DENIED:PASS` and `LOGIN:GRAPHICAL:PASS`, and
//! `LOGIN:LOGOUT:PASS session=<id>`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use user::messenger::{
    self, accounts, confd, errno, logind, registry, router, services, Endpoint, Message, Parcel,
};
use user::sys;

/// The confd key that selects the session kind.
pub const SESSION_MODE_KEY: &str = "sys/session/mode";
/// The [`SESSION_MODE_KEY`] values.
pub const GRAPHICAL: &str = "graphical";
pub const CONSOLE: &str = "console";
/// `init`'s registry ids for the desktop shell and the login screen.
const SHELL_APP: &str = "lazyshell";
const GREETER_APP: &str = "greeter";
/// How long a call to `confd`, `accountsd` or `init` may wait (PIT ticks).
const CALL_TICKS: u64 = 300;
/// How long to wait before trying again what could not be done (a service
/// not registered yet, a launch refused for a full task table).
const RETRY_TICKS: u64 = 100;
/// The account the build logs straight in (`LAZYOS_AUTOLOGIN`, resolved by
/// `user/build.rs`): empty for none.
const AUTOLOGIN: &str = env!("LAZYOS_AUTOLOGIN_NAME");

/// Whether this boot's logins are graphical (see the module docs).
pub fn requested() -> bool {
    let configured = confd::Client::connect().ok().and_then(|client| {
        match client.with_timeout(CALL_TICKS).get(SESSION_MODE_KEY) {
            Ok(Some(::confd::Value::Str(mode))) => Some(mode),
            _ => None,
        }
    });
    match configured.as_deref() {
        Some(GRAPHICAL) => true,
        Some(CONSOLE) => false,
        _ => cfg!(lazyos_desktop),
    }
}

/// The open desktop session.
struct Active {
    /// Index into the session table.
    index: usize,
    id: u64,
}

/// What to do once a request has been answered.
enum After {
    Nothing,
    /// A login opened a session: the login screen goes.
    StopGreeter,
    /// The session ended: tell everyone, and bring the login screen back.
    EndSession,
}

/// The graphical login state.
struct Desktop {
    sessions: Vec<logind::SessionRecord>,
    active: Option<Active>,
    next_session: u64,
    bus: Option<router::Bus>,
    accounts: Option<Endpoint>,
    init: Option<Endpoint>,
    greeter: bool,
    autologin_tried: bool,
}

/// Serve graphical logins until the system stops. Returns only when the login
/// screen cannot be started at all, for the caller's console fallback.
pub fn run(server: &Endpoint, buffer: &mut [u8]) -> messenger::Result<()> {
    let mut desktop = Desktop {
        sessions: Vec::new(),
        active: None,
        next_session: 0,
        bus: None,
        accounts: None,
        init: None,
        greeter: false,
        autologin_tried: false,
    };
    sys::write_str("logind: graphical login\n");
    loop {
        desktop.connect();
        let mut retry = false;
        if desktop.active.is_none() && !desktop.greeter {
            if !desktop.autologin_tried && desktop.accounts.is_some() {
                desktop.autologin_tried = true;
                desktop.autologin();
            }
            if desktop.active.is_none() && desktop.autologin_tried {
                match desktop.show_greeter() {
                    Ok(()) => {}
                    Err(true) => retry = true,
                    Err(false) => return Ok(()),
                }
            }
            retry |= desktop.accounts.is_none();
        }
        let deadline = retry.then(|| sys::clock() + RETRY_TICKS);
        let message = match server.recv_with(buffer, deadline) {
            Ok(message) => message,
            Err(messenger::Error::Errno(code)) if code == -errno::ETIMEDOUT => continue,
            Err(error) => return Err(error),
        };
        let (reply, after) = desktop.answer(&message);
        if let Some(txn) = message.txn {
            server.reply_or_drop(txn, &reply)?;
        }
        match after {
            After::Nothing => {}
            After::StopGreeter => desktop.stop_greeter(),
            After::EndSession => desktop.end_session(),
        }
    }
}

impl Desktop {
    /// Find the services this needs, as they appear.
    fn connect(&mut self) {
        if self.accounts.is_none() {
            self.accounts = registry::resolve(accounts::NAME).ok();
        }
        if self.bus.is_none() {
            self.bus = router::Bus::connect(services::INIT_NAME).ok();
        }
        if self.init.is_none() {
            self.init = services::resolve_service(services::INIT_NAME).ok();
        }
    }

    /// Log the build's autologin account in, through the same path a typed
    /// login takes (without the password: the build asked for it).
    fn autologin(&mut self) {
        if AUTOLOGIN.is_empty() {
            return;
        }
        let account = self
            .accounts
            .as_ref()
            .and_then(|endpoint| accounts::lookup_name(endpoint, AUTOLOGIN).ok().flatten());
        let Some(account) = account else {
            sys::write_str(&format!(
                "LOGIN:AUTOLOGIN:FAIL user={AUTOLOGIN} reason=unknown-user\n"
            ));
            return;
        };
        match self.open(&account) {
            Ok(_) => sys::write_str(&format!("LOGIN:AUTOLOGIN:PASS user={AUTOLOGIN}\n")),
            Err(reason) => sys::write_str(&format!(
                "LOGIN:AUTOLOGIN:FAIL user={AUTOLOGIN} reason={reason}\n"
            )),
        }
    }

    /// Have `init` start the login screen. `Err(true)`: try again later;
    /// `Err(false)`: the image has none, give graphical logins up.
    fn show_greeter(&mut self) -> Result<(), bool> {
        let init = self.init.as_ref().ok_or(true)?;
        let deadline = Some(sys::clock() + CALL_TICKS);
        match services::launch_by(init, GREETER_APP, "", 0, deadline) {
            Ok(result) => {
                sys::write_str(&format!("LOGIN:GREETER:PASS pid={}\n", result.pid));
                self.greeter = true;
                Ok(())
            }
            Err(messenger::Error::Init(code)) if code == errno::ENOENT => {
                sys::write_str(
                    "LOGIN:GRAPHICAL:FAIL reason=no-login-screen; starting the console login\n",
                );
                Err(false)
            }
            Err(_) => Err(true),
        }
    }

    fn stop_greeter(&mut self) {
        if let Some(init) = self.init.as_ref() {
            let _ = services::stop(init, GREETER_APP);
        }
        self.greeter = false;
    }

    /// Answer one request, and say what must follow the reply.
    fn answer(&mut self, message: &Message) -> (Parcel, After) {
        let method = message.method();
        if message.interface_id() != logind::INTERFACE {
            return (Parcel::default(), After::Nothing);
        }
        match method {
            logind::wire::METHOD_SESSIONS => {
                let active = self.active.iter().count() as u64;
                let reply = logind::sessions_reply(&self.sessions, active).unwrap_or_default();
                (reply, After::Nothing)
            }
            logind::wire::METHOD_LOGIN => match self.login(message) {
                Ok(session) => {
                    let body = logind::wire::encode_login_reply(&logind::wire::LoginReply {
                        session,
                    })
                    .unwrap_or_default();
                    (logind::reply(method, body), After::StopGreeter)
                }
                Err((code, why)) => (logind::error_reply(method, code, why), After::Nothing),
            },
            logind::wire::METHOD_LOGOUT => {
                let caller = message.caller();
                match self.active.as_ref() {
                    Some(active) if caller.session == active.id && active.id != 0 => {
                        let body = logind::wire::encode_logout_reply(
                            &logind::wire::LogoutReply { session: active.id },
                        )
                        .unwrap_or_default();
                        (logind::reply(method, body), After::EndSession)
                    }
                    _ => (
                        logind::error_reply(method, errno::EPERM, "not your session"),
                        After::Nothing,
                    ),
                }
            }
            _ => (
                logind::error_reply(method, errno::EINVAL, "unknown method"),
                After::Nothing,
            ),
        }
    }

    /// `Login` from the login screen: who may ask, then the password, then
    /// the session.
    fn login(&mut self, message: &Message) -> Result<u64, (i64, &'static str)> {
        let caller = message.caller();
        // The login screen's identity alone: a task of a session (or anyone
        // else) never gets to try passwords through this call.
        if caller.uid != logind::GREETER_UID || caller.label_id != 0 || caller.session != 0 {
            return Err((errno::EPERM, "only the login screen may log in"));
        }
        if self.active.is_some() {
            return Err((errno::EBUSY, "a session is open"));
        }
        let args = logind::wire::decode_login_args(&message.parcel.body)
            .map_err(|_| (errno::EINVAL, "malformed"))?;
        let endpoint = self
            .accounts
            .as_ref()
            .ok_or((errno::EAGAIN, "accounts not ready"))?;
        let account = match accounts::lookup_name(endpoint, &args.user) {
            Ok(Some(account)) => account,
            Ok(None) => {
                super::deny(&mut self.bus, &args.user, "unknown-user");
                return Err((errno::EACCES, "refused"));
            }
            Err(_) => {
                super::deny(&mut self.bus, &args.user, "no-accounts");
                return Err((errno::EACCES, "refused"));
            }
        };
        if !accounts::authenticate(endpoint, &args.user, &args.secret).unwrap_or(false) {
            super::deny(&mut self.bus, &args.user, "bad-secret");
            return Err((errno::EACCES, "refused"));
        }
        self.open(&account).map_err(|_| {
            super::deny(&mut self.bus, &args.user, "spawn-failed");
            (errno::EIO, "the desktop could not start")
        })
    }

    /// Open a desktop session for `account`: announce it, launch LazyShell
    /// into it, record it. The one path every login takes.
    fn open(&mut self, account: &accounts::UserRecord) -> Result<u64, String> {
        self.next_session += 1;
        let id = self.next_session;
        // The owner `init` stamps the shell with (and the environment it gives
        // the session's apps), announced before the launch.
        publish_session(&mut self.bus, account, id, 0, "starting");
        let launched = match self.init.as_ref() {
            Some(init) => {
                services::launch_by(init, SHELL_APP, "", id, Some(sys::clock() + CALL_TICKS))
                    .map_err(|error| error_text(&error))
            }
            None => Err(String::from("init-unreachable")),
        };
        let pid = match launched {
            Ok(result) => result.pid,
            Err(reason) => {
                sys::write_str(&format!(
                    "LOGIN:GRAPHICAL:FAIL session={id} reason={reason}\n"
                ));
                publish_end(&mut self.bus, account, id, 1);
                return Err(reason);
            }
        };
        self.sessions.push(logind::SessionRecord {
            id,
            user: account.name.clone(),
            uid: account.uid,
            pid,
            state: String::from("active"),
            started: sys::clock(),
        });
        self.active = Some(Active {
            index: self.sessions.len() - 1,
            id,
        });
        sys::write_str(&format!(
            "LOGIN:OK:PASS user={} uid={} session={id} pid={pid}\n",
            account.name, account.uid
        ));
        sys::write_str(&format!(
            "LOGIN:GRAPHICAL:PASS user={} session={id} app={SHELL_APP} pid={pid}\n",
            account.name
        ));
        if let Some(bus) = self.bus.as_mut() {
            let start = logind::wire::LoginStart {
                user: account.name.clone(),
                uid: account.uid,
                session: id,
                pid,
                state: String::from("active"),
            };
            let _ = logind::wire::publish_system_events_login_start(bus, &start);
        }
        publish_session(&mut self.bus, account, id, pid, "active");
        Ok(id)
    }

    /// End the active session: `init` stops its tasks on the event, and the
    /// next loop pass brings the login screen back.
    fn end_session(&mut self) {
        let Some(active) = self.active.take() else {
            return;
        };
        let record = &mut self.sessions[active.index];
        record.state = String::from("exited");
        sys::write_str(&format!(
            "LOGIN:LOGOUT:PASS session={} user={}\n",
            active.id, record.user
        ));
        let account = accounts::UserRecord {
            name: record.user.clone(),
            uid: record.uid,
            ..accounts::UserRecord::default()
        };
        publish_end(&mut self.bus, &account, active.id, 0);
        publish_session(&mut self.bus, &account, active.id, 0, "exited");
    }
}

/// Publish the retained `system/events/login/session/<id>` record. It carries
/// the account's home, so `init` can give the session's apps their
/// environment without looking the account up again (issue #508).
pub fn publish_session(
    bus: &mut Option<router::Bus>,
    account: &accounts::UserRecord,
    session: u64,
    pid: u64,
    state: &str,
) {
    if let Some(bus) = bus.as_mut() {
        let record = logind::wire::LoginSession {
            user: account.name.clone(),
            uid: account.uid,
            pid,
            state: String::from(state),
            home: account.home.clone(),
        };
        let id = format!("{session}");
        let _ = logind::wire::publish_system_events_login_session(bus, &id, &record);
    }
}

/// Publish `system/events/login/end` for `session`.
fn publish_end(
    bus: &mut Option<router::Bus>,
    account: &accounts::UserRecord,
    session: u64,
    status: u64,
) {
    if let Some(bus) = bus.as_mut() {
        let event = logind::wire::LoginEnd {
            user: account.name.clone(),
            uid: account.uid,
            session,
            status,
        };
        let _ = logind::wire::publish_system_events_login_end(bus, &event);
    }
}

/// A short reason for a failed `Launch`.
fn error_text(error: &messenger::Error) -> String {
    match error {
        messenger::Error::Errno(code) => format!("errno{code}"),
        messenger::Error::Init(code) => format!("init{code}"),
        _ => String::from("launch-failed"),
    }
}
