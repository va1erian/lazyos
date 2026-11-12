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
use libmessenger::client::{CallError, Deadline, Errno};
use libmessenger::server::{Caller, Fault};
use user::central;
use user::messenger::accounts::Directory;
use user::messenger::client::Native;
use user::messenger::confd::ValueClient;
use user::messenger::services::init::{Client as InitClient, ClientError as InitError};
use user::trace;

use user::messenger::{self, accounts, confd, errno, logind, Endpoint};
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
/// How soon a refused login-screen launch is tried again: `init` holds it
/// while a logout's apps are still quitting (issue #651, at most 3.5 s), and
/// the screen should come back as soon as they are gone.
const GREETER_RETRY_TICKS: u64 = 20;
/// The account the build logs straight in (`LAZYOS_AUTOLOGIN`, resolved by
/// `user/build.rs`): empty for none.
const AUTOLOGIN: &str = env!("LAZYOS_AUTOLOGIN_NAME");

/// Whether this boot's logins are graphical (see the module docs).
pub fn requested() -> bool {
    let configured = confd::connect().ok().and_then(|client| {
        match client.get_value_within(SESSION_MODE_KEY, Deadline::ticks(CALL_TICKS)) {
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
    bus: Option<central::Publisher>,
    accounts: Option<accounts::Client>,
    init: Option<InitClient>,
    greeter: bool,
    autologin_tried: bool,
    /// What the request just answered asks for once its reply is sent.
    after: After,
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
        after: After::Nothing,
    };
    sys::write_str("logind: graphical login\n");
    loop {
        desktop.connect();
        let mut retry = false;
        let mut soon = false;
        if desktop.active.is_none() && !desktop.greeter {
            if !desktop.autologin_tried && desktop.accounts.is_some() {
                desktop.autologin_tried = true;
                desktop.autologin();
            }
            if desktop.active.is_none() && desktop.autologin_tried {
                match desktop.show_greeter() {
                    Ok(()) => {}
                    Err(true) => soon = true,
                    Err(false) => return Ok(()),
                }
            }
            retry |= desktop.accounts.is_none();
        }
        let deadline = if soon {
            Some(sys::clock() + GREETER_RETRY_TICKS)
        } else {
            retry.then(|| sys::clock() + RETRY_TICKS)
        };
        let message = match server.recv_with(buffer, deadline) {
            Ok(message) => message,
            Err(messenger::Error::Errno(code)) if code == -errno::ETIMEDOUT => continue,
            Err(error) => return Err(error),
        };
        let reply = logind::wire::dispatch(&mut desktop, &message);
        if let Some(txn) = message.txn {
            server.reply_or_drop(txn, &reply)?;
        }
        match core::mem::replace(&mut desktop.after, After::Nothing) {
            After::Nothing => {}
            After::StopGreeter => desktop.stop_greeter(),
            After::EndSession => desktop.end_session(),
        }
    }
}

/// The generated server: a method that must be followed by more work (the
/// greeter stops, the session ends) leaves it in `after` for the loop.
impl logind::wire::Server for Desktop {
    fn sessions(&mut self, _: &Caller) -> Result<logind::wire::SessionsReply, Fault> {
        Ok(logind::wire::SessionsReply {
            active: self.active.iter().count() as u64,
            sessions: self.sessions.clone(),
        })
    }

    fn login(&mut self, cx: &Caller, user: String, secret: String) -> Result<u64, Fault> {
        let session = Desktop::login(self, cx, &user, &secret)
            .map_err(|(code, why)| Fault::new(code as u32, why))?;
        self.after = After::StopGreeter;
        Ok(session)
    }

    fn logout(&mut self, cx: &Caller) -> Result<u64, Fault> {
        match self.active.as_ref() {
            Some(active) if cx.session == active.id && active.id != 0 => {
                let id = active.id;
                // The end is announced before the answer: if the broker will
                // not take it, `init` would never stop the session's tasks,
                // so refuse and keep the session.
                if !self.announce_end() {
                    trace::line!(
                        trace::Channel::Login,
                        "LOGOUT:FAIL session={id} reason=unannounced"
                    );
                    return Err(Fault::refuse(Errno::AGAIN, "cannot announce the logout"));
                }
                self.after = After::EndSession;
                Ok(id)
            }
            _ => Err(Fault::refuse(Errno::PERM, "not your session")),
        }
    }
}

impl Desktop {
    /// Find the services this needs, as they appear.
    fn connect(&mut self) {
        if self.accounts.is_none() {
            self.accounts = accounts::Client::try_connect(Native).ok();
        }
        if self.bus.is_none() {
            self.bus = Some(central::Publisher::new());
        }
        if self.init.is_none() {
            self.init = InitClient::try_connect(Native).ok();
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
            .and_then(|client| client.lookup_name(AUTOLOGIN).ok().flatten());
        let Some(account) = account else {
            trace::line!(
                trace::Channel::Login,
                "AUTOLOGIN:FAIL user={AUTOLOGIN} reason=unknown-user"
            );
            return;
        };
        match self.open(&account) {
            Ok(_) => trace::line!(trace::Channel::Login, "AUTOLOGIN:PASS user={AUTOLOGIN}"),
            Err(reason) => trace::line!(
                trace::Channel::Login,
                "AUTOLOGIN:FAIL user={AUTOLOGIN} reason={reason}"
            ),
        }
    }

    /// Have `init` start the login screen. `Err(true)`: try again later;
    /// `Err(false)`: the image has none, give graphical logins up.
    fn show_greeter(&mut self) -> Result<(), bool> {
        let init = self.init.as_ref().ok_or(true)?;
        match init.launch_within(GREETER_APP, "", 0, Deadline::ticks(CALL_TICKS)) {
            Ok(result) => {
                trace::line!(trace::Channel::Login, "GREETER:PASS pid={}", result.pid);
                self.greeter = true;
                Ok(())
            }
            Err(error) if error.code() == -errno::ENOENT => {
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
            let _ = init.stop(GREETER_APP);
        }
        self.greeter = false;
    }

    /// `Login` from the login screen: who may ask, then the password, then
    /// the session.
    fn login(
        &mut self,
        caller: &Caller,
        user: &str,
        secret: &str,
    ) -> Result<u64, (i64, &'static str)> {
        // The login screen's identity alone: a task of a session (or anyone
        // else) never gets to try passwords through this call.
        if caller.uid != logind::GREETER_UID || caller.label_id != 0 || caller.session != 0 {
            return Err((errno::EPERM, "only the login screen may log in"));
        }
        if self.active.is_some() {
            return Err((errno::EBUSY, "a session is open"));
        }
        let accounts = self
            .accounts
            .as_ref()
            .ok_or((errno::EAGAIN, "accounts not ready"))?;
        let account = match accounts.lookup_name(user) {
            Ok(Some(account)) => account,
            Ok(None) => {
                super::deny(&mut self.bus, user, "unknown-user");
                return Err((errno::EACCES, "refused"));
            }
            Err(_) => {
                super::deny(&mut self.bus, user, "no-accounts");
                return Err((errno::EACCES, "refused"));
            }
        };
        match accounts.authenticate(user, secret) {
            Ok(true) => {}
            // `accountsd`'s brake holds for this name (U1): say so, and do
            // not count it as one more wrong password.
            Err(error) if error.code() == -errno::EAGAIN => {
                super::deny(&mut self.bus, user, "slowed");
                return Err((errno::EAGAIN, "too many failed attempts"));
            }
            Ok(false) | Err(_) => {
                super::deny(&mut self.bus, user, "bad-secret");
                return Err((errno::EACCES, "refused"));
            }
        }
        self.open(&account).map_err(|_| {
            super::deny(&mut self.bus, user, "spawn-failed");
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
            Some(init) => init
                .launch_within(SHELL_APP, "", id, Deadline::ticks(CALL_TICKS))
                .map_err(|error| error_text(&error)),
            None => Err(String::from("init-unreachable")),
        };
        let pid = match launched {
            Ok(result) => result.pid,
            Err(reason) => {
                trace::line!(
                    trace::Channel::Login,
                    "GRAPHICAL:FAIL session={id} reason={reason}"
                );
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
        trace::line!(
            trace::Channel::Login,
            "OK:PASS user={} uid={} session={id} pid={pid}",
            account.name,
            account.uid
        );
        trace::line!(
            trace::Channel::Login,
            "GRAPHICAL:PASS user={} session={id} app={SHELL_APP} pid={pid}",
            account.name
        );
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

    /// The active session's account, as the end and session records carry it.
    fn active_account(&self) -> Option<(u64, usize, accounts::UserRecord)> {
        let active = self.active.as_ref()?;
        let record = &self.sessions[active.index];
        let account = accounts::UserRecord {
            name: record.user.clone(),
            uid: record.uid,
            ..accounts::UserRecord::default()
        };
        Some((active.id, active.index, account))
    }

    /// Publish the active session's `login/end` (`init` stops its tasks on
    /// it); whether the broker took it.
    fn announce_end(&mut self) -> bool {
        let Some((id, _, account)) = self.active_account() else {
            return false;
        };
        publish_end(&mut self.bus, &account, id, 0)
    }

    /// Finish the logout `Logout` announced: the next loop pass brings the
    /// login screen back.
    fn end_session(&mut self) {
        let Some((id, index, account)) = self.active_account() else {
            return;
        };
        self.active = None;
        self.sessions[index].state = String::from("exited");
        trace::line!(
            trace::Channel::Login,
            "LOGOUT:PASS session={id} user={}",
            account.name
        );
        publish_session(&mut self.bus, &account, id, 0, "exited");
    }
}

/// Publish the retained `system/events/login/session/<id>` record. It carries
/// the account's home, so `init` can give the session's apps their
/// environment without looking the account up again (issue #508).
pub fn publish_session(
    bus: &mut Option<central::Publisher>,
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

/// Publish `system/events/login/end` for `session`; whether the broker took
/// it (`init` stops the session's tasks on it).
fn publish_end(
    bus: &mut Option<central::Publisher>,
    account: &accounts::UserRecord,
    session: u64,
    status: u64,
) -> bool {
    let Some(bus) = bus.as_mut() else {
        return false;
    };
    let event = logind::wire::LoginEnd {
        user: account.name.clone(),
        uid: account.uid,
        session,
        status,
    };
    logind::wire::publish_system_events_login_end(bus, &event).is_ok()
}

/// A short reason for a failed `Launch`.
fn error_text(error: &InitError) -> String {
    match error {
        CallError::Transport(errno) => format!("errno{}", errno.to_syscall()),
        CallError::Remote { code, .. } => format!("init{code}"),
        CallError::Service(never) => match *never {},
    }
}
