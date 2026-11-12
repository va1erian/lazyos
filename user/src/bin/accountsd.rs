//! `accountsd` (`/system/bin/accountsd`): the account database service
//! (issues #101, #508, #624; docs/accounts-plan.md U1).
//!
//! It runs as the `_accounts` system uid with no capability and owns the
//! account database, `/accounts/db` (`libs/accountdb`): every account,
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
//! the change as root on this service's request alone. At every start each
//! account's home is checked the same way (`manage::ensure_homes`): made if
//! missing, handed over if root owns it (a volume from before U1, when
//! `admin` was uid 0), archived and made afresh if another uid owns it.

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
use user::trace;

use accountdb::ratelimit::Limiter;
use accountdb::{Db, User};
use accounts::wire;
use libmessenger::client::{Deadline, Errno};
use libmessenger::server::{Caller, Fault};
use user::messenger::client::Native;
use user::messenger::services::health::Client as HealthClient;
use user::messenger::{self, accounts, errno, registry, Error};
use user::sys::{self, Cred};

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
            trace::line!(
                trace::Channel::Accounts,
                "LOAD:PASS rows={} admins={} file={}",
                db.users.len(),
                db.admins(),
                fhs::state::ACCOUNTS_DB
            );
            store::write_views(db);
            manage::ensure_homes(db);
            if db.needs_setup() {
                trace::line!(trace::Channel::Accounts, "SETUP:NEEDED no account yet");
            }
        }
        Err(reason) => trace::line!(
            trace::Channel::Accounts,
            "LOAD:FAIL reason={reason} file={}; no account is served",
            fhs::state::ACCOUNTS_DB
        ),
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
        let reply = wire::dispatch(&mut Handler(&mut state), &message);
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
    HealthClient::try_connect(Native)
        .and_then(|healthd| {
            healthd.report_within("accountsd", status, &detail, Deadline::ticks(HEALTH_TICKS))
        })
        .is_ok()
}

/// A refusal: an errno-style `code` and the friendly text the user reads.
pub(crate) struct Refusal(pub(crate) i64, pub(crate) String);

impl Refusal {
    pub(crate) fn new(code: i64, text: &str) -> Refusal {
        Refusal(code, text.to_string())
    }
}

/// The service state answering requests (the generated [`wire::Server`]).
struct Handler<'a>(&'a mut State);

/// The sender's stamped credentials, as the policy code reads them.
fn cred_of(cx: &Caller) -> Cred {
    Cred::new(cx.uid, cx.gid, cx.caps, cx.label_id, cx.session)
}

impl Handler<'_> {
    /// A daemon without accounts (no valid account file, issue #508) answers
    /// every request with `EIO` and the reason, so a caller can tell "no
    /// account database" from "no such user".
    fn ready(&self) -> Result<(), Fault> {
        match &self.0.db {
            Ok(_) => Ok(()),
            Err(reason) => Err(Fault::refuse(
                Errno::IO,
                &format!(
                    "no account database ({}: {reason})",
                    fhs::state::ACCOUNTS_DB
                ),
            )),
        }
    }
}

impl wire::Server for Handler<'_> {
    fn lookup(
        &mut self,
        _: &Caller,
        name: Option<String>,
        uid: Option<u32>,
    ) -> Result<wire::LookupReply, Fault> {
        self.ready()?;
        let db = database(self.0)?;
        let found = match (name, uid) {
            (Some(name), _) => db.user(&name),
            (None, Some(uid)) => db.by_uid(uid),
            (None, None) => None,
        };
        let user = found.map(record);
        Ok(wire::LookupReply {
            found: user.is_some(),
            user,
        })
    }

    fn list_users(&mut self, _: &Caller) -> Result<wire::ListUsersReply, Fault> {
        self.ready()?;
        let db = database(self.0)?;
        Ok(wire::ListUsersReply {
            users: db.users.iter().map(record).collect(),
            setup: db.needs_setup(),
        })
    }

    fn authenticate(&mut self, cx: &Caller, name: String, secret: String) -> Result<bool, Fault> {
        self.ready()?;
        Ok(authn::authenticate(self.0, &cred_of(cx), &name, &secret)?)
    }

    fn create(
        &mut self,
        cx: &Caller,
        name: String,
        secret: String,
        admin: bool,
    ) -> Result<accounts::UserRecord, Fault> {
        self.ready()?;
        let args = wire::CreateArgs {
            name,
            secret,
            admin,
        };
        Ok(manage::create(self.0, cred_of(cx), &args)?)
    }

    fn delete(&mut self, cx: &Caller, name: String, home: String) -> Result<(), Fault> {
        self.ready()?;
        Ok(manage::delete(
            self.0,
            cred_of(cx),
            &wire::DeleteArgs { name, home },
        )?)
    }

    fn set_password(
        &mut self,
        cx: &Caller,
        name: String,
        old: Option<String>,
        secret: String,
    ) -> Result<(), Fault> {
        self.ready()?;
        let args = wire::SetPasswordArgs { name, old, secret };
        Ok(manage::set_password(self.0, cred_of(cx), &args)?)
    }

    fn set_admin(&mut self, cx: &Caller, name: String, admin: bool) -> Result<(), Fault> {
        self.ready()?;
        Ok(manage::set_admin(
            self.0,
            cred_of(cx),
            &wire::SetAdminArgs { name, admin },
        )?)
    }
}

impl From<Refusal> for Fault {
    fn from(Refusal(code, text): Refusal) -> Fault {
        Fault::new(code as u32, &text)
    }
}

/// The loaded database (`answer` already refused when there is none).
fn database(state: &State) -> Result<&Db, Refusal> {
    state
        .db
        .as_ref()
        .map_err(|_| Refusal::new(errno::EIO, "no account database"))
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
