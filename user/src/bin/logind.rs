//! `logind` (`/system/bin/logind`): console login and sessions (issue #101).
//!
//! The S3 console login from `docs/security-model.md` section 3, as far as this
//! branch can carry it:
//!
//! 1. **identify**: prompt on the terminal (`login:`) and read a line;
//! 2. **authenticate**: ask `accountsd` to check the secret (which delegates to
//!    `keyd` when present);
//! 3. **create a session**: mint a session id, publish
//!    `system/events/login/*` through the supervisor's topic router, and keep
//!    the session in the table `messengerctl sessions` renders;
//! 4. **start the shell as the user**: the native `creds` spawn stamps the
//!    child with `uid/gid/session` *before it can run*, so the shell never even
//!    briefly owns the default root identity;
//! 5. **audit**: every attempt prints `LOGIN:OK:PASS` / `LOGIN:DENIED:PASS`
//!    (the machine-parseable lines a headless session captures) and publishes
//!    to `system/events/login/{start,denied,end}`, which `logd` records.
//!
//! A graphical session (issue #157, `graphical.rs`) replaces step 4: when the
//! confd key `sys/session/mode` is `graphical`, `init` launches the desktop
//! shell (LazyShell) into the session instead, and a failure falls back to the
//! console shell. The console path is unchanged.
//!
//! Failed attempts wait [`FAIL_DELAY_TICKS`] before the next prompt (the
//! documented rate-limit), and the session capabilities are empty
//! ([`SESSION_CAPS`]): a console session starts with no ambient authority, and
//! later S3 slices grant its compositor/clipboard/topic set explicitly.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "logind/graphical.rs"]
mod graphical;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::messenger::{self, accounts, logind, registry, router, services, wait, Endpoint, Parcel};
use user::sys::{self, Cred};

/// How long to wait before looking for the accounts service again (PIT
/// ticks). `init` starts `logind` after `accountsd`, so this only
/// covers an `accountsd` still registering or restarting.
const ACCOUNTS_RETRY_TICKS: u64 = 5;
/// Failed-login backoff (PIT ticks, 100 Hz); the friendly face of rate limiting.
const FAIL_DELAY_TICKS: u64 = 30;
/// Capabilities a console session starts with. Empty today: least privilege is
/// the default and the session's grants arrive with the compositor/clipboard
/// slice. `init` holds the set-credentials capability and may grant more later.
const SESSION_CAPS: u32 = 0;
/// Longest name/secret line the prompt accepts.
const LINE_MAX: usize = 64;
/// The login screen's help: the default accounts of `/system/etc/passwd`
/// (`build_support/passwd`), documented in `docs/security-model.md` section 3.
/// Bring-up plaintext secrets until #447 hashes them.
const LOGIN_HELP: &str = "Default accounts: admin (password nimda, uid 0) and \
                          user (password lazy, uid 1000).\n";

/// The session whose shell is currently running. A graphical session's shell
/// is `init`'s child, not ours, so its exit is never reaped here: the session
/// stays active (there is no logout yet).
struct ActiveSession {
    /// Index into the session table.
    index: usize,
    /// Task slot of the shell.
    pid: u64,
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("logind: console login (issue #101)\n");
    if let Err(error) = run() {
        sys::write_str("logind: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

/// Register the session table and run console logins until the system ends.
fn run() -> messenger::Result<()> {
    let (published, server) = messenger::create_pair()?;
    registry::register(logind::NAME, &published, &[logind::INTERFACE], 0)?;
    sys::write_str("logind: waiting for the accounts service\n");
    let mut accountsd: Option<Endpoint> = None;
    let mut bus: Option<router::Bus> = None;
    let mut sessions: Vec<logind::SessionRecord> = Vec::new();
    let mut active: Option<ActiveSession> = None;
    let mut next_session = 0u64;
    // Reused receive buffer: the user bump allocator never reclaims per-call
    // buffers, so long-lived loops must not allocate one per message.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];

    loop {
        // Answer `Sessions` queries even while a shell runs.
        serve_queries(&server, &sessions, &mut buffer)?;
        if accountsd.is_none() {
            accountsd = registry::resolve(accounts::NAME).ok();
            if accountsd.is_some() {
                sys::write_str("logind: accounts service ready; login enabled\n");
                sys::write_str(LOGIN_HELP);
            }
        }
        if bus.is_none() {
            bus = router::Bus::connect(services::INIT_NAME).ok();
        }

        if let Some(current) = active.as_ref() {
            // Park until a query or the session shell's exit (the child
            // bell, P7): queries are answered at once, with no poll.
            let ready = wait::wait_any(&[server], wait::WAIT_CHILD, None)?;
            if ready & wait::CHILD_READY != 0 {
                if let Some((pid, status)) = sys::wait(sys::clock().max(1)) {
                    if pid == current.pid {
                        end_session(&mut sessions, current, status, &mut bus);
                        active = None;
                    }
                }
            }
            continue;
        }

        let Some(endpoint) = accountsd.as_ref() else {
            // No accounts service yet: wait for it to register.
            sleep(ACCOUNTS_RETRY_TICKS);
            continue;
        };
        if let Some(started) = prompt_login(endpoint, &mut sessions, &mut next_session, &mut bus) {
            active = Some(started);
        }
    }
}

/// Run the identify/authenticate/spawn pipeline for one console attempt.
fn prompt_login(
    endpoint: &Endpoint,
    sessions: &mut Vec<logind::SessionRecord>,
    next_session: &mut u64,
    bus: &mut Option<router::Bus>,
) -> Option<ActiveSession> {
    sys::write_str("\nLazyOS login: ");
    let name = read_line(true);
    if name.is_empty() {
        return None;
    }
    sys::write_str("Password: ");
    let secret = read_line(false);
    sys::write_str("\n");

    let user = match accounts::lookup_name(endpoint, &name) {
        Ok(Some(user)) => user,
        Ok(None) => {
            deny(bus, &name, "unknown-user");
            return None;
        }
        Err(_) => {
            // `accountsd` has no account file (or cannot be reached): no login
            // can succeed, and saying "wrong password" would be a lie.
            sys::write_str(&format!(
                "Login unavailable: the account database ({}) did not load.\n\
                 This system needs recovery; see docs/security-model.md section 3.\n",
                fhs::etc::PASSWD
            ));
            deny(bus, &name, "no-accounts");
            return None;
        }
    };
    let matched = accounts::authenticate(endpoint, &name, &secret).unwrap_or(false);
    if !matched {
        deny(bus, &name, "bad-secret");
        return None;
    }

    // Mint the session the shell will own. The id both scopes the credential
    // (kernel-side) and names the session table row and topic
    // (`system/events/login/session/<id>`), which is how the rest of the
    // system learns about it through Messenger.
    *next_session += 1;
    let id = *next_session;
    // A graphical session: `init` launches the desktop shell into it.
    let desktop = if graphical::requested() {
        graphical::start(bus, &user, id)
    } else {
        None
    };
    let pid = match desktop {
        Some(pid) => pid,
        None => match spawn_console_shell(&user, id) {
            Some(pid) => pid,
            None => {
                deny(bus, &name, "spawn-failed");
                return None;
            }
        },
    };
    let started = sys::clock();
    sessions.push(logind::SessionRecord {
        id,
        user: user.name.clone(),
        uid: user.uid,
        pid,
        state: String::from("active"),
        started,
    });
    let index = sessions.len() - 1;
    sys::write_str(&format!(
        "LOGIN:OK:PASS user={} uid={} session={id} pid={pid}\n",
        user.name, user.uid
    ));
    if let Some(bus) = bus.as_mut() {
        let start = logind::wire::LoginStart {
            user: user.name.clone(),
            uid: user.uid,
            session: id,
            pid,
            state: String::from("active"),
        };
        let _ = logind::wire::publish_system_events_login_start(bus, &start);
    }
    graphical::publish_session(bus, &user, id, pid, "active");
    Some(ActiveSession { index, pid })
}

/// Spawn the user's console shell stamped with the session's credentials.
/// The passwd shell field is the bare `sh` (issue #254): the Linux personality
/// makes the kernel alias `sh` to the shipped BusyBox. The environment is the
/// session's ([`accounts::session_env`]: `HOME`, `USER`, `PATH`).
///
/// A login starts in the home directory, as on any Unix. `logind` is native and
/// has no working directory to hand down, so a short `-c` script changes to
/// `$HOME` and then `exec`s the login shell (`$0`, the passwd field, passed as
/// an argument so it is never parsed as shell text). A missing home is
/// reported and the shell starts in `/`.
fn spawn_console_shell(user: &accounts::UserRecord, session: u64) -> Option<u64> {
    let cred = Cred::new(user.uid, user.gid, SESSION_CAPS, 0, session);
    let env = accounts::session_env(&user.name, &user.home);
    let env: Vec<&str> = env.iter().map(String::as_str).collect();
    let shell = user.shell.as_str();
    sys::spawnv(
        shell,
        &[shell, "-c", LOGIN_SCRIPT, shell],
        &env,
        sys::Personality::Linux,
        sys::SpawnCred::As(cred),
    )
    .ok()
}

/// The console login's `-c` script (see [`spawn_console_shell`]).
const LOGIN_SCRIPT: &str =
    "cd \"$HOME\" 2>/dev/null || echo \"login: no home directory $HOME; starting in /\"; exec \"$0\"";

/// Record a refused attempt, print its serial marker, and rate-limit the next
/// prompt.
fn deny(bus: &mut Option<router::Bus>, name: &str, reason: &str) {
    sys::write_str(&format!("LOGIN:DENIED:PASS user={name} reason={reason}\n"));
    if let Some(bus) = bus.as_mut() {
        let event = logind::wire::LoginDenied {
            user: String::from(name),
            reason: String::from(reason),
        };
        let _ = logind::wire::publish_system_events_login_denied(bus, &event);
    }
    // Rate limit: a failed attempt costs the next one a visible pause.
    sleep(FAIL_DELAY_TICKS);
}

/// Mark a session ended and publish the logout event.
fn end_session(
    sessions: &mut [logind::SessionRecord],
    active: &ActiveSession,
    status: u64,
    bus: &mut Option<router::Bus>,
) {
    sessions[active.index].state = String::from("exited");
    let record = &sessions[active.index];
    sys::write_str(&format!(
        "logind: session {} for {} ended (status {status})\n",
        record.id, record.user
    ));
    if let Some(bus) = bus.as_mut() {
        let event = logind::wire::LoginEnd {
            user: record.user.clone(),
            uid: record.uid,
            session: record.id,
            status,
        };
        let _ = logind::wire::publish_system_events_login_end(bus, &event);
    }
}

/// Answer queued `Sessions` queries without blocking.
fn serve_queries(
    server: &Endpoint,
    sessions: &[logind::SessionRecord],
    buffer: &mut [u8],
) -> messenger::Result<()> {
    while let Some(message) = server.poll_recv_with(buffer)? {
        let reply = if message.interface_id() == logind::INTERFACE
            && message.method() == logind::wire::METHOD_SESSIONS
        {
            let active = sessions
                .iter()
                .filter(|session| session.state == "active")
                .count() as u64;
            logind::sessions_reply(sessions, active).unwrap_or_default()
        } else {
            Parcel::default()
        };
        if let Some(txn) = message.txn {
            server.reply_or_drop(txn, &reply)?;
        }
    }
    Ok(())
}

/// Read one line from the terminal. `echo` prints the characters back (the
/// username and dialog do; the password does not).
fn read_line(echo: bool) -> String {
    let mut line = [0u8; LINE_MAX];
    let mut len = 0usize;
    loop {
        let ch = sys::read_char();
        if ch == b'\n' as u64 {
            if echo {
                sys::write_str("\n");
            }
            return String::from(core::str::from_utf8(&line[..len]).unwrap_or(""));
        }
        if ch == 8 {
            if len > 0 {
                len -= 1;
                if echo {
                    sys::write_str("\u{8} \u{8}");
                }
            }
            continue;
        }
        if (32..127).contains(&ch) && len + 1 < line.len() {
            line[len] = ch as u8;
            len += 1;
            if echo {
                sys::write(&[ch as u8]);
            }
        }
    }
}

/// Sleep `ticks` PIT ticks (10 ms each) on the native sleep.
fn sleep(ticks: u64) {
    sys::sleep_ns(ticks * 10_000_000);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
