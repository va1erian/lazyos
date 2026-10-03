//! `accountsd` (`/system/bin/accountsd`): the account database service
//! (issues #101, #508).
//!
//! S3 accounts, per `docs/security-model.md` section 3: users, groups, home
//! dirs and password verifiers. This slice answers the questions `logind`
//! needs and nothing else:
//!
//! * `Lookup(name/uid)` -- the passwd fields (name, uid, gid, home, shell);
//! * `Authenticate(name, secret)` -- verified through `keyd` when it is
//!   registered, otherwise through the documented bring-up verifier;
//! * `Create` -- declared, answered `ENOSYS`: account management is the next
//!   iteration (docs/filesystem-plan.md section 5).
//!
//! # Where the database lives
//!
//! `/system/etc/passwd` (`fhs::etc::PASSWD`, `name:uid:gid:secret:home:shell`)
//! is the **only** account source; there is no built-in table. The image
//! ships `admin` (uid 0) and `user` (uid 1000). The file is parsed strictly
//! (`libs/passwd`) and the daemon **fails closed**: when it is missing,
//! unreadable, larger than `PASSWD_MAX`, has a malformed row or reuses a name
//! or uid, the daemon prints `ACCOUNTS:LOAD:FAIL reason=<...>`, reports health
//! `failed`, and answers every request with an error instead of an account.
//! `logind` then refuses every login with a message saying so. That is a
//! recovery situation, never a machine with a default password (#447). A good
//! load prints `ACCOUNTS:LOAD:PASS rows=<n>`.
//!
//! # The secret
//!
//! `keyd` owns password verifiers (`Argon2id`, `SHARE_ONLY` buffers). Until
//! #447 moves hashes to `/system/etc/shadow`, the file carries a plaintext
//! secret: this is a **bring-up fallback, not a password hash**. See
//! [`verify_secret`].

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::files;
use user::messenger::{self, accounts, errno, keyd, registry, services, Error, Message, Parcel};
use user::sys;

/// How long the service waits for a request before retrying an undelivered
/// health report (PIT ticks, 100 Hz).
const HEALTH_RETRY_TICKS: u64 = 50;
/// Deadline for one health report, so a busy `healthd` cannot stall logins.
const HEALTH_TICKS: u64 = 10;
/// `ENOENT` and `EFBIG` from `user::files`.
const ENOENT: i64 = 2;
const EFBIG: i64 = 27;

/// One account: the public record plus the secret verifier.
struct Account {
    record: accounts::UserRecord,
    verifier: String,
}

impl From<passwd::Entry> for Account {
    fn from(entry: passwd::Entry) -> Account {
        Account {
            record: accounts::UserRecord {
                name: entry.name,
                uid: entry.uid,
                gid: entry.gid,
                home: entry.home,
                shell: entry.shell,
            },
            verifier: entry.secret,
        }
    }
}

/// The accounts, or why there are none.
type Table = Result<Vec<Account>, String>;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("accountsd: account database (issue #101)\n");
    if let Err(error) = run() {
        sys::write_str("accountsd: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

/// Register the service and answer account queries for the life of the system.
fn run() -> messenger::Result<()> {
    let (published, server) = messenger::create_pair()?;
    registry::register(accounts::NAME, &published, &[accounts::INTERFACE], 0)?;
    let table = load_table();
    match &table {
        Ok(rows) => sys::write_str(&format!(
            "ACCOUNTS:LOAD:PASS rows={} file={}\n",
            rows.len(),
            fhs::etc::PASSWD
        )),
        Err(reason) => sys::write_str(&format!(
            "ACCOUNTS:LOAD:FAIL reason={reason} file={}; no account is served\n",
            fhs::etc::PASSWD
        )),
    }
    let mut health_sent = false;
    // Printed once, the first time a delegation to keyd is attempted (see
    // `verify_secret`); purely informational, so it does not gate anything.
    let mut keyd_seen = false;
    // Reused receive buffer: the user bump allocator never reclaims per-call
    // buffers, so the service loop must not allocate one per message.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];

    loop {
        if !health_sent {
            health_sent = report_health(&table);
        }
        let deadline = (!health_sent).then(|| sys::clock() + HEALTH_RETRY_TICKS);
        let message = match server.recv_with(&mut buffer, deadline) {
            Ok(message) => message,
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => continue,
            Err(error) => return Err(error),
        };
        let reply = answer(&table, &mut keyd_seen, &message);
        if let Some(txn) = message.txn {
            server.reply_or_drop(txn, &reply)?;
        }
    }
}

/// Read and parse the account file. `Err` carries the `reason=` text.
fn load_table() -> Table {
    let path = fhs::etc::PASSWD;
    let bytes = match files::read_up_to(path, passwd::PASSWD_MAX) {
        Ok(bytes) => bytes,
        Err(ENOENT) => return Err(passwd::LoadError::Missing.to_string()),
        Err(EFBIG) => {
            let size = files::stat(path)
                .map(|(size, _)| size as usize)
                .unwrap_or(0);
            return Err(passwd::LoadError::Oversize(size).to_string());
        }
        Err(code) => return Err(passwd::LoadError::Unreadable(code).to_string()),
    };
    passwd::parse(&bytes)
        .map(|entries| entries.into_iter().map(Account::from).collect())
        .map_err(|error| error.to_string())
}

/// Tell `healthd` whether accounts loaded. Returns whether it was delivered.
fn report_health(table: &Table) -> bool {
    let (status, detail) = match table {
        Ok(rows) => ("ok", format!("rows={}", rows.len())),
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

/// The reply to one request: served from the table, or refused when there
/// are no accounts or the request is malformed (a malformed request still gets
/// an answer, or its caller would wait forever).
fn answer(table: &Table, keyd_seen: &mut bool, message: &Message) -> Parcel {
    let method = message.method();
    let rows = match table {
        Ok(rows) => rows,
        Err(reason) => {
            let text = format!("no account database ({}: {reason})", fhs::etc::PASSWD);
            return accounts::error_reply(method, errno::EIO, &text);
        }
    };
    match dispatch(rows, keyd_seen, message) {
        Ok(reply) => reply,
        Err(Error::Errno(code)) => accounts::error_reply(method, -code, "refused"),
        Err(error) => accounts::error_reply(method, errno::EINVAL, error.message()),
    }
}

/// Find an account by name.
fn by_name<'a>(table: &'a [Account], name: &str) -> Option<&'a Account> {
    table.iter().find(|account| account.record.name == name)
}

/// Find an account by uid.
fn by_uid(table: &[Account], uid: u32) -> Option<&Account> {
    table.iter().find(|account| account.record.uid == uid)
}

/// Whether `secret` authenticates `account`.
///
/// `keyd` is the real verifier (`docs/security-model.md` section 3): when the
/// service is registered, the secret goes there and its verdict wins. Until
/// `keyd` exists this is the **bring-up fallback**: compare against the table's
/// plaintext verifier. It is a stand-in so the login path can be exercised; no
/// real deployment may use it.
///
/// Every call re-resolves `keyd` and re-provisions *this* account before
/// asking it to verify, rather than caching "keyd is present" from the first
/// call: caching it meant a `keyd` that registered after the first login
/// attempt, or a `keyd` that crashed and restarted with an empty table, was
/// never (re-)told about any account and every delegated login failed
/// permanently. `provision` is a cheap upsert (`keyd` replaces the verifier by
/// name), so re-provisioning on every login is correct, not just tolerated.
fn verify_secret(account: &Account, secret: &str, keyd_seen: &mut bool) -> bool {
    let Ok(client) = keyd::Client::connect() else {
        if !*keyd_seen {
            sys::write_str(
                "accountsd: keyd absent; bring-up verifier (see docs/security-model.md section 3)\n",
            );
        }
        // A plain comparison is the fallback's whole definition; it is
        // plaintext by construction and is documented as bring-up-only.
        return account.verifier == secret;
    };
    if !*keyd_seen {
        *keyd_seen = true;
        sys::write_str("accountsd: password verification delegated to keyd\n");
    }
    if let Err(error) = client.provision(&account.record.name, &account.verifier) {
        sys::write_str(&format!(
            "accountsd: keyd refused to provision {}: {}\n",
            account.record.name,
            error.message()
        ));
        // Fall through and ask anyway: keyd may already know this account from
        // an earlier successful provision, and the verdict is authoritative.
    }
    // keyd is authoritative once it is reachable: an error from `verify` (a
    // malformed request, an internal refusal, anything short of "wrong
    // password") is a deny, never a reason to fall back to the plaintext
    // bring-up verifier. Falling back here would let a caller who can force
    // `verify` to error (e.g. an oversized secret) authenticate against the
    // weaker plaintext comparison instead of Argon2id. The plaintext fallback
    // exists only for the "keyd is not registered at all" case above.
    client.verify(&account.record.name, secret).unwrap_or(false)
}

/// Answer one request from the loaded table.
fn dispatch(
    table: &[Account],
    keyd_seen: &mut bool,
    message: &Message,
) -> messenger::Result<Parcel> {
    if message.interface_id() != accounts::INTERFACE {
        return Err(Error::Errno(-errno::EINVAL));
    }
    match message.method() {
        accounts::wire::METHOD_LOOKUP => {
            let (name, uid) = accounts::decode_lookup(&message.parcel)?;
            let found = match (name, uid) {
                (Some(name), _) => by_name(table, &name),
                (None, Some(uid)) => by_uid(table, uid),
                (None, None) => None,
            };
            accounts::user_reply(found.map(|account| &account.record))
        }
        accounts::wire::METHOD_AUTHENTICATE => {
            let (name, secret) = accounts::decode_authenticate(&message.parcel)?;
            let matched = by_name(table, &name)
                .map(|account| verify_secret(account, &secret, keyd_seen))
                .unwrap_or(false);
            accounts::auth_reply(matched)
        }
        // Account management is the next iteration; the accounts are the
        // file's, and only an update of the image changes them.
        accounts::wire::METHOD_CREATE => Err(Error::Errno(-errno::ENOSYS)),
        _ => Err(Error::Errno(-errno::EINVAL)),
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
