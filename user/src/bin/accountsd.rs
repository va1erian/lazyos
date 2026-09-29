//! `accountsd` (`ACCTD.ELF`): the account database service (issue #101).
//!
//! S3 accounts, per `docs/security-model.md` section 3: users, groups, home
//! dirs and password verifiers. This slice answers the questions `logind`
//! needs and nothing else:
//!
//! * `Lookup(name/uid)` -- the `/etc/passwd` fields (name, uid, gid, home,
//!   shell);
//! * `Authenticate(name, secret)` -- verified through `keyd` when it is
//!   registered, otherwise through the documented bring-up verifier;
//! * `CreateUser` -- admin-only: the kernel-stamped credentials of the
//!   Messenger sender must be uid 0, which the daemon reads with the native
//!   `creds` get op (it holds `CAP_SETUID` as a system service).
//!
//! # Where the database lives
//!
//! The writable store does not exist in this branch, and the only filesystem is
//! the read-only boot volume, so the fallback named by the issue is used: a
//! built-in table, optionally overridden by a passwd-style file
//! (`name:uid:gid:secret:home:shell`) on the boot volume. `CreateUser` updates
//! the in-memory table; persistence through the store plugs in where
//! [`WRITABLE_STORE`] flips, exactly as `logd` marks its store decision.
//!
//! # The secret
//!
//! `keyd` owns password verifiers (`Argon2id`, `SHARE_ONLY` buffers) once it
//! exists. Until then the file carries a plaintext secret and the daemon
//! compares bytes: this is a **bring-up fallback, not a password hash**, and it
//! exists only so logind can be exercised end to end. See the module comment on
//! [`verify_secret`].

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::messenger::{self, accounts, keyd, registry, Error, Message, Parcel};
use user::sys;

/// The passwd-style file read from the boot volume, if present.
const PASSWD_FILE: &[u8] = b"PASSWD\0";
/// Largest passwd file the daemon reads.
const PASSWD_MAX: usize = 1024;
/// The built-in table when no file is present (the same content the image
/// carries as `PASSWD`, so a boot without the file behaves the same).
const BUILTIN: &str = "root:0:0:toor:/root:/SH.ELF\nalice:1000:1000:lazy:/home/alice:/SH.ELF\n";
/// See the module docs: no writable volume exists in this branch, so updates
/// stay in memory. Flipping this to `true` (S3) persists them.
const WRITABLE_STORE: bool = false;

/// One account: the public record plus the secret verifier.
struct Account {
    record: accounts::UserRecord,
    verifier: String,
}

impl Account {
    /// The `name:uid:gid:secret:home:shell` line form.
    fn parse(line: &str) -> Option<Account> {
        let mut fields = line.split(':');
        let name = fields.next()?.trim();
        let uid: u32 = fields.next()?.trim().parse().ok()?;
        let gid: u32 = fields.next()?.trim().parse().ok()?;
        let secret = fields.next()?.trim();
        let home = fields.next().unwrap_or("/").trim();
        let shell = fields.next().unwrap_or("SH.ELF").trim();
        if name.is_empty() {
            return None;
        }
        Some(Account {
            record: accounts::UserRecord {
                name: name.to_string(),
                uid,
                gid,
                home: home.to_string(),
                shell: shell.to_string(),
            },
            verifier: secret.to_string(),
        })
    }
}

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
    let (mut table, from_file) = load_table();
    sys::write_str(&alloc::format!(
        "accountsd: {} account(s) from {} (issue #101)\n",
        table.len(),
        if from_file {
            "PASSWD"
        } else if WRITABLE_STORE {
            "the writable store"
        } else {
            "the built-in bring-up table"
        }
    ));
    // Printed once, the first time a delegation to keyd is attempted (see
    // `verify_secret`); purely informational, so it does not gate anything.
    let mut keyd_seen = false;
    // Reused receive buffer: the user bump allocator never reclaims per-call
    // buffers, so the service loop must not allocate one per message.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];

    loop {
        let message = server.recv_with(&mut buffer, None)?;
        // A malformed request still gets an answer, or its caller would wait
        // forever; an empty parcel fails the caller's decode.
        let reply = dispatch(&mut table, &mut keyd_seen, &message).unwrap_or_default();
        if let Some(txn) = message.txn {
            server.reply_or_drop(txn, &reply)?;
        }
    }
}

/// Load the boot-volume passwd file when present, else the built-in table.
/// Returns the table and whether it came from the file.
fn load_table() -> (Vec<Account>, bool) {
    let mut bytes = alloc::vec![0u8; PASSWD_MAX];
    let table: Vec<Account> = if let Some(length) = sys::read_file(PASSWD_FILE, &mut bytes) {
        let text = core::str::from_utf8(&bytes[..length]).unwrap_or("");
        text.lines().filter_map(Account::parse).collect()
    } else {
        Vec::new()
    };
    if table.is_empty() {
        (BUILTIN.lines().filter_map(Account::parse).collect(), false)
    } else {
        (table, true)
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

/// Push `account`'s verifier into `keyd` over an already-resolved client.
fn provision_one(client: &keyd::Client, account: &Account) -> messenger::Result<()> {
    client.provision(&account.record.name, &account.verifier)
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
    if let Err(error) = provision_one(&client, account) {
        sys::write_str(&alloc::format!(
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

/// Whether the Messenger sender is an administrator (uid 0).
///
/// The kernel stamps the sender slot; reading its credentials requires
/// `CAP_SETUID`, which this system service holds. A refusal or a read error is
/// "not an admin".
fn sender_is_admin(sender: u64) -> bool {
    let mut cred = sys::Cred::default();
    match sys::cred_get(Some(sender), &mut cred) {
        Ok(()) => cred.uid == 0,
        Err(_) => false,
    }
}

/// Answer one request.
fn dispatch(
    table: &mut Vec<Account>,
    keyd_seen: &mut bool,
    message: &Message,
) -> messenger::Result<Parcel> {
    if message.interface_id() != accounts::INTERFACE {
        return Err(Error::Errno(-messenger::errno::EINVAL));
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
        accounts::wire::METHOD_CREATE => {
            let new_user = accounts::decode_create(&message.parcel)?;
            if !sender_is_admin(message.sender) {
                return accounts::create_reply(false, "admin only: uid 0 required");
            }
            if new_user.name.is_empty() {
                return accounts::create_reply(false, "name required");
            }
            if new_user.uid == 0 {
                return accounts::create_reply(false, "uid 0 is reserved for the system");
            }
            if by_name(table, &new_user.name).is_some() {
                return accounts::create_reply(false, "name already exists");
            }
            if by_uid(table, new_user.uid).is_some() {
                return accounts::create_reply(false, "uid already exists");
            }
            let record = accounts::UserRecord {
                name: new_user.name.clone(),
                uid: new_user.uid,
                gid: new_user.gid,
                home: new_user.home.clone(),
                shell: new_user.shell.clone(),
            };
            let account = Account {
                record,
                verifier: new_user.secret,
            };
            // A user created while `keyd` is active must be known to it too,
            // or `keyd`'s verdict (authoritative once it exists) would refuse
            // every login for an account the table nonetheless claims exists.
            // Reject creation rather than accept a permanently locked-out
            // account: `keyd` being merely absent (not yet booted) is fine and
            // falls through to the plaintext bring-up fallback as usual.
            if let Ok(client) = keyd::Client::connect() {
                if let Err(error) = provision_one(&client, &account) {
                    sys::write_str(&alloc::format!(
                        "accountsd: keyd refused to provision {}: {}; account not created\n",
                        account.record.name,
                        error.message()
                    ));
                    return accounts::create_reply(
                        false,
                        "keyd rejected the account's password; not created",
                    );
                }
            }
            table.push(account);
            sys::write_str(&alloc::format!(
                "accountsd: created {} (uid {})\n",
                new_user.name,
                new_user.uid
            ));
            accounts::create_reply(true, "created")
        }
        _ => Err(Error::Errno(-messenger::errno::EINVAL)),
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
