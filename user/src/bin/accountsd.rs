//! `accountsd` (`/system/bin/accountsd`): the account database service
//! (issues #101, #508, #624; docs/accounts-plan.md U1).
//!
//! It runs as the `_accounts` system uid with no capability and owns the
//! account database, `/conf/accounts/db` (`libs/accountdb`): every account,
//! group and password verifier. It serves `os.lazy.accounts.v1`:
//!
//! * `Lookup`, `ListUsers`: the public fields (name, uid, gid, home, shell,
//!   admin), open to every caller;
//! * `Authenticate(name, secret)`: verified by `keyd`, slowed per account name
//!   and per caller (`accountdb::ratelimit`, `authn.rs`);
//! * `Create`, `Delete`, `SetAdmin`, and `SetPassword` for another user:
//!   from `elevd` alone (an administrator approved them on the trusted
//!   prompt), or the first account from the login screen during the
//!   first-boot setup; a user changes their own password with the old one
//!   (`accountdb::policy`, `manage.rs`).
//!
//! # The database
//!
//! Loaded at start, written back whole after every change (a temporary file
//! renamed over it, `store.rs`), and mirrored into the world-readable views
//! `/system/etc/passwd` and `/system/etc/group` (owned by `_accounts`), which
//! are rewritten at start too, so an image update that put the build's seed
//! views back is corrected at once. The parser is strict and the daemon
//! **fails closed**: a missing or damaged database prints
//! `ACCOUNTS:LOAD:FAIL reason=<...>`, reports health `failed`, and answers
//! every request with an error instead of an account; `logind` then refuses
//! every login. That is a recovery situation, never a machine with a default
//! password (#447). A good load prints `ACCOUNTS:LOAD:PASS rows=<n>`.
//!
//! # Secrets and homes
//!
//! `keyd` derives and checks every verifier (`Provision` returns the new one
//! for the database); this service never sees a password beyond passing it
//! on, and there is no fallback when `keyd` cannot answer. Homes are made
//! (0700, from `/system/etc/skel`), archived or removed by `init`, which runs
//! the change as root on this service's request alone.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "accountsd/authn.rs"]
mod authn;
#[path = "accountsd/manage.rs"]
mod manage;
#[path = "accountsd/store.rs"]
mod store;

use alloc::format;
use alloc::string::{String, ToString};
use core::panic::PanicInfo;

use accountdb::ratelimit::Limiter;
use accountdb::{Db, User};
use user::messenger::{self, accounts, errno, registry, services, Error, Message, Parcel};
use user::sys;

/// How long the service waits for a request before retrying an undelivered
/// health report (PIT ticks, 100 Hz).
const HEALTH_RETRY_TICKS: u64 = 50;
/// Deadline for one health report, so a busy `healthd` cannot stall logins.
const HEALTH_TICKS: u64 = 10;

/// The service state: the database (or why there is none) and the brake.
pub(crate) struct State {
    pub(crate) db: Result<Db, String>,
    pub(crate) limiter: Limiter,
    /// Whether `keyd`'s absence was already reported.
    pub(crate) keyd_warned: bool,
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("accountsd: account database (issue #624)\n");
    if let Err(error) = run() {
        sys::write_str("accountsd: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

/// Register the service and answer account requests for the life of the system.
fn run() -> messenger::Result<()> {
    let (published, server) = messenger::create_pair()?;
    registry::register(accounts::NAME, &published, &[accounts::INTERFACE], 0)?;
    // Serving: what waits for this service may start (init.Ready, P7.3).
    user::messenger::services::init::notify_ready();
    let mut state = State {
        db: store::load(),
        limiter: Limiter::new(),
        keyd_warned: false,
    };
    match &state.db {
        Ok(db) => {
            sys::write_str(&format!(
                "ACCOUNTS:LOAD:PASS rows={} admins={} file={}\n",
                db.users.len(),
                db.admins(),
                fhs::state::ACCOUNTS_DB
            ));
            store::write_views(db);
            if db.needs_setup() {
                sys::write_str("ACCOUNTS:SETUP:NEEDED no account yet\n");
            }
        }
        Err(reason) => sys::write_str(&format!(
            "ACCOUNTS:LOAD:FAIL reason={reason} file={}; no account is served\n",
            fhs::state::ACCOUNTS_DB
        )),
    }
    let mut health_sent = false;
    // Reused receive buffer: the user bump allocator never reclaims per-call
    // buffers, so the service loop must not allocate one per message.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];

    loop {
        if !health_sent {
            health_sent = report_health(&state.db);
        }
        let deadline = (!health_sent).then(|| sys::clock() + HEALTH_RETRY_TICKS);
        let message = match server.recv_with(&mut buffer, deadline) {
            Ok(message) => message,
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => continue,
            Err(error) => return Err(error),
        };
        let reply = answer(&mut state, &message);
        if let Some(txn) = message.txn {
            server.reply_or_drop(txn, &reply)?;
        }
    }
}

/// Tell `healthd` whether accounts loaded. Returns whether it was delivered.
fn report_health(db: &Result<Db, String>) -> bool {
    let (status, detail) = match db {
        Ok(db) => ("ok", format!("rows={}", db.users.len())),
        Err(reason) => ("failed", format!("no accounts: {reason}")),
    };
    let Ok(endpoint) = services::resolve_service(services::HEALTHD_NAME) else {
        return false;
    };
    let Ok(request) = services::health_report_request("accountsd", status, &detail) else {
        return false;
    };
    endpoint
        .call(&request, Some(sys::clock() + HEALTH_TICKS))
        .is_ok()
}

/// A refusal: an errno-style `code` and the friendly text the user reads.
pub(crate) struct Refusal(pub(crate) i64, pub(crate) String);

impl Refusal {
    pub(crate) fn new(code: i64, text: &str) -> Refusal {
        Refusal(code, text.to_string())
    }
}

/// The reply to one request: served from the database, or refused when there
/// is none or the request is malformed (a malformed request still gets an
/// answer, or its caller would wait forever).
fn answer(state: &mut State, message: &Message) -> Parcel {
    let method = message.method();
    if let Err(reason) = &state.db {
        let text = format!(
            "no account database ({}: {reason})",
            fhs::state::ACCOUNTS_DB
        );
        return accounts::error_reply(method, errno::EIO, &text);
    }
    if message.interface_id() != accounts::INTERFACE {
        return accounts::error_reply(method, errno::EINVAL, "not an accounts request");
    }
    match dispatch(state, message) {
        Ok(reply) => reply,
        Err(Refusal(code, text)) => accounts::error_reply(method, code, &text),
    }
}

/// Answer one request.
fn dispatch(state: &mut State, message: &Message) -> Result<Parcel, Refusal> {
    let body = &message.parcel.body;
    let malformed = |_| Refusal::new(errno::EINVAL, "malformed request");
    match message.method() {
        accounts::wire::METHOD_LOOKUP => {
            let args = accounts::wire::decode_lookup_args(body).map_err(malformed)?;
            let db = database(state)?;
            let found = match (args.name, args.uid) {
                (Some(name), _) => db.user(&name),
                (None, Some(uid)) => db.by_uid(uid),
                (None, None) => None,
            };
            let record = found.map(record);
            accounts::user_reply(record.as_ref()).map_err(|_| Refusal::new(errno::EIO, "encode"))
        }
        accounts::wire::METHOD_LISTUSERS => {
            let db = database(state)?;
            let reply = accounts::wire::ListUsersReply {
                users: db.users.iter().map(record).collect(),
                setup: db.needs_setup(),
            };
            encoded(message, accounts::wire::encode_list_users_reply(&reply))
        }
        accounts::wire::METHOD_AUTHENTICATE => {
            let args = accounts::wire::decode_authenticate_args(body).map_err(malformed)?;
            let ok = authn::authenticate(state, &message.caller(), &args.name, &args.secret)?;
            accounts::auth_reply(ok).map_err(|_| Refusal::new(errno::EIO, "encode"))
        }
        accounts::wire::METHOD_CREATE => {
            let args = accounts::wire::decode_create_args(body).map_err(malformed)?;
            let user = manage::create(state, message, &args)?;
            let reply = accounts::wire::CreateReply { user };
            encoded(message, accounts::wire::encode_create_reply(&reply))
        }
        accounts::wire::METHOD_DELETE => {
            let args = accounts::wire::decode_delete_args(body).map_err(malformed)?;
            manage::delete(state, message, &args)?;
            Ok(accounts::reply(message.method(), alloc::vec::Vec::new()))
        }
        accounts::wire::METHOD_SETPASSWORD => {
            let args = accounts::wire::decode_set_password_args(body).map_err(malformed)?;
            manage::set_password(state, message, &args)?;
            Ok(accounts::reply(message.method(), alloc::vec::Vec::new()))
        }
        accounts::wire::METHOD_SETADMIN => {
            let args = accounts::wire::decode_set_admin_args(body).map_err(malformed)?;
            manage::set_admin(state, message, &args)?;
            Ok(accounts::reply(message.method(), alloc::vec::Vec::new()))
        }
        _ => Err(Refusal::new(errno::EINVAL, "unknown method")),
    }
}

/// The loaded database (`answer` already refused when there is none).
fn database(state: &State) -> Result<&Db, Refusal> {
    state
        .db
        .as_ref()
        .map_err(|_| Refusal::new(errno::EIO, "no account database"))
}

/// An encoded reply body as a parcel of the request's method.
fn encoded(
    message: &Message,
    body: Result<alloc::vec::Vec<u8>, libmessenger::Error>,
) -> Result<Parcel, Refusal> {
    body.map(|body| accounts::reply(message.method(), body))
        .map_err(|_| Refusal::new(errno::EIO, "encode"))
}

/// The public record of `user`.
pub(crate) fn record(user: &User) -> accounts::UserRecord {
    accounts::UserRecord {
        name: user.name.clone(),
        uid: user.uid,
        gid: user.gid,
        home: user.home.clone(),
        shell: user.shell.clone(),
        admin: user.is_admin(),
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
