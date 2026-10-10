//! `keyd` (`/system/bin/keyd`): the secrets and crypto service (issue #102).
//!
//! `docs/security-model.md` section 8 defines this service: `keyd` owns all
//! long-term secrets (password verifiers first, signing/wrapping keys next),
//! clients never receive raw key material, and every primitive comes from a
//! vetted crate. This program is the S3 implementation:
//!
//! * it holds password verifiers (Argon2id) and symmetric keys in its own
//!   address space;
//! * it serves `Verify(user, secret)`, `Sign(key, digest)`, `Wrap(key, bytes)`,
//!   `Unwrap(key, blob)`, `Random(len)`, `GenerateKey(type)`, plus a `List`
//!   that exposes key **ids and counters** and never material;
//! * it seeds an entropy pool from RDRAND when CPUID offers it, else from PIT
//!   tick and TSC timing jitter (`lazyos_crypto::rng`);
//! * it proves itself at boot with machine-parseable serial markers:
//!   `KEYD:SELFTEST:PASS`, or `KEYD:SELFTEST:FAIL:<detail>` plus `KEYD:READY`
//!   once it is registered.
//!
//! # Isolation status (the honest version)
//!
//! The key table never leaves this task: a shared buffer is mapped by whoever
//! holds a handle to it (the kernel has no share-only buffer for services,
//! `docs/messenger-core-plan.md` section 1), so `keyd` never puts key material
//! in one. The property holds structurally instead: the wire protocol has no
//! "read key material" method, and replies carry only ids, tags, blobs and
//! counters ([`Keyd::keys`]).
//!
//! # Provisioning
//!
//! At boot `keyd` loads the accounts' verifiers from the account database,
//! `/accounts/db` (docs/accounts-plan.md U1; the image build seeds it),
//! all or nothing: without it every login fails closed. `accountsd` asks
//! `Verify` for each login and `keyd`'s verdict is final; there is no
//! plaintext anywhere and no demo account. Every account method is accepted
//! from `accountsd`'s own identity alone (the `_accounts` system uid,
//! unlabelled; no capability or uid 0 is enough): `Verify`, because
//! `accountsd` slows password guessing and a direct check would get around
//! that brake; `Provision` (install or replace a verifier, returning it for
//! the database), `Restore` (put back the database's verifier when that
//! write failed) and `Forget`, because whoever may plant a verifier may
//! become that user. Every verifier is Argon2id under
//! [`kdf::Params::INTERACTIVE`].
//!
//! The key methods are open to every caller and scoped to the uid that
//! generated the key (taken from the sender's kernel-stamped credentials):
//! `Sign`, `Wrap`, `Unwrap` and `List` only see the caller's own keys, and
//! `Generate` makes one owned by the caller. `Random` and `Ping` hold no
//! secret of anyone's.
//!
//! # Known follow-ups (out of this change's scope)
//!
//! * `getrandom` on the Linux ABI is not wired: the Linux syscall table lives
//!   in `kernel/src/arch/linux.rs`, outside `libs/crypto` and this binary. A
//!   thin wrapper that forwards to `keyd`'s `Random` is the follow-up.
//! * Argon2id runs from a reusable [`kdf::Arena`] allocated once at boot, so
//!   the memory cost is paid once (the native heap is only about 1.875 MiB,
//!   see `libs/crypto`). Raising `m_cost` to the RFC's 64 MiB is the follow-up
//!   that needs a reclaiming allocator or a bigger native heap.
//! * Argon2id (0.5.3) links but emits a future-incompat warning for its AVX2
//!   `target_feature` on `x86_64-unknown-none`; argon2 0.6 resolves it upstream.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::messenger::keyd::wire as api;
use user::messenger::{self, errno, keyd as wire, registry, Error, Message, Parcel};
use user::sys;

#[path = "keyd/secrets.rs"]
mod secrets;
#[path = "keyd/selftest.rs"]
mod selftest;
#[path = "keyd/state.rs"]
mod state;

use secrets::{denied_error, Secrets};
use secretstore::{authorize, Caller, Op, Owner};
use selftest::{self_test, SELF_TEST_OWNER};
use state::{Keyd, MAX_BYTES, MAX_TEXT};

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("keyd: secrets and crypto service (issue #102)\n");
    if let Err(error) = run() {
        sys::write_str("keyd: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

/// Boot, self-test, register, then serve requests forever.
fn run() -> messenger::Result<()> {
    let mut keyd = Keyd::new().ok_or(Error::Errno(-errno::ENOMEM))?;
    keyd.seed_entropy();
    if !keyd.entropy.is_seeded() {
        sys::write_str("KEYD:SELFTEST:FAIL:no entropy source\n");
        sys::exit(1);
    }
    load_db(&mut keyd);
    let mut secrets = Secrets::load(&mut keyd.entropy);
    match self_test(&mut keyd) {
        Ok(()) => sys::write_str("KEYD:SELFTEST:PASS\n"),
        // Keep serving: some operations may still be usable, and `init` would
        // restart a service that exits, hiding the failure in a crash loop.
        Err(detail) => sys::write_str(&format!("KEYD:SELFTEST:FAIL:{detail}\n")),
    }
    // Scrub the self-test's own keys and account before this instance starts
    // taking real requests: they must not consume the serving instance's
    // MAX_ACCOUNTS/MAX_KEYS_PER_OWNER capacity or be visible to a real client.
    // Whatever the self-test reached (pass or fail), root has generated no
    // keys but its own at this point, so this cannot remove anything else's.
    keyd.forget_keys_owned_by(SELF_TEST_OWNER);
    keyd.forget_account("selftest-user");

    let (published, server) = messenger::create_pair()?;
    registry::register(wire::NAME, &published, &[wire::INTERFACE], 0)?;
    sys::write_str("KEYD:READY\n");
    // Serving: what waits for this service may start (init.Ready, P7.3).
    user::messenger::services::init::notify_ready();

    // One receive buffer for the whole life of the service: the user bump
    // allocator never reclaims per-call buffers, so a long-lived loop must not
    // allocate one per request.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    loop {
        let message = server.recv_with(&mut buffer, None)?;
        let method = message.method();
        let reply = match dispatch(&mut keyd, &mut secrets, &message) {
            Ok(reply) => reply,
            Err(error) => wire::error_reply(method, error),
        };
        if let Some(txn) = message.txn {
            server.reply_or_drop(txn, &reply)?;
        }
    }
}

/// Load the account verifiers from the account database (U1), which only
/// `_accounts` (and root) can read. Without it (or with a damaged one) `keyd`
/// serves on with no account, so every login fails closed: there is no
/// plaintext or built-in fallback. Prints `KEYD:SHADOW:PASS rows=<n>` or
/// `KEYD:SHADOW:FAIL reason=<...>`.
fn load_db(keyd: &mut Keyd) {
    let path = fhs::state::ACCOUNTS_DB;
    let loaded = match user::files::read_up_to(path, accountdb::DB_MAX) {
        Ok(bytes) => keyd.load_db(&bytes),
        Err(2) => Err(String::from("missing")),
        Err(code) => Err(format!("unreadable errno={code}")),
    };
    match loaded {
        Ok(rows) => sys::write_str(&format!("KEYD:SHADOW:PASS rows={rows} file={path}\n")),
        Err(reason) => sys::write_str(&format!(
            "KEYD:SHADOW:FAIL reason={reason} file={path}; no account can log in\n"
        )),
    }
}

/// Whether `message` comes from the accounts service: its system uid,
/// unlabelled and outside any session (kernel-stamped, so unforgeable).
fn from_accountsd(message: &Message) -> bool {
    let caller = message.caller();
    caller.uid == accountdb::ACCOUNTS_UID && caller.label_id == 0 && caller.session == 0
}

/// Dispatch one request; errors become error replies at the call site.
fn dispatch(
    keyd: &mut Keyd,
    secrets: &mut Secrets,
    message: &Message,
) -> messenger::Result<Parcel> {
    if message.interface_id() != wire::INTERFACE {
        return Err(Error::Errno(-errno::EINVAL));
    }
    let body = &message.parcel.body;
    let parse = Error::Parcel;
    match message.method() {
        api::METHOD_VERIFY => {
            // A password check is `accountsd`'s alone: it slows guessers
            // (`Authenticate`'s brake), and an open `Verify` would be a way
            // around that brake for any session (review of #659, H2).
            if !from_accountsd(message) {
                return Err(Error::Errno(-errno::EPERM));
            }
            let args = api::decode_verify_args(body).map_err(parse)?;
            let user = bounded_text(args.user)?;
            let secret = bounded_text(args.secret)?;
            let ok = keyd.verify(&user, &secret);
            reply(
                api::METHOD_VERIFY,
                api::encode_verify_reply(&api::VerifyReply { ok }),
            )
        }
        api::METHOD_SIGN => {
            let owner = caller_uid(message)?;
            let args = api::decode_sign_args(body).map_err(parse)?;
            let digest = bounded_bytes(args.digest, 128)?;
            let tag = keyd.sign(args.key, owner, &digest)?;
            reply(
                api::METHOD_SIGN,
                api::encode_sign_reply(&api::SignReply { tag: tag.to_vec() }),
            )
        }
        api::METHOD_WRAP => {
            let owner = caller_uid(message)?;
            let args = api::decode_wrap_args(body).map_err(parse)?;
            let plaintext = bounded_bytes(args.plaintext, MAX_BYTES)?;
            let blob = keyd.wrap(args.key, owner, &plaintext)?;
            reply(
                api::METHOD_WRAP,
                api::encode_wrap_reply(&api::WrapReply { blob }),
            )
        }
        api::METHOD_UNWRAP => {
            let owner = caller_uid(message)?;
            let args = api::decode_unwrap_args(body).map_err(parse)?;
            let blob = bounded_bytes(args.blob, MAX_BYTES)?;
            let plaintext = keyd.unwrap(args.key, owner, &blob)?;
            reply(
                api::METHOD_UNWRAP,
                api::encode_unwrap_reply(&api::UnwrapReply { plaintext }),
            )
        }
        api::METHOD_RANDOM => {
            let args = api::decode_random_args(body).map_err(parse)?;
            let bytes = keyd.random(args.len as usize)?;
            reply(
                api::METHOD_RANDOM,
                api::encode_random_reply(&api::RandomReply { bytes }),
            )
        }
        api::METHOD_PROVISION => {
            // Whoever can plant a verifier can log in as that account: the
            // accounts service alone, by its identity (U1). No capability or
            // uid is enough on its own.
            if !from_accountsd(message) {
                return Err(Error::Errno(-errno::EPERM));
            }
            let args = api::decode_provision_args(body).map_err(parse)?;
            let user = bounded_text(args.user)?;
            let secret = bounded_text(args.secret)?;
            let verifier = keyd.provision(&user, &secret)?;
            reply(
                api::METHOD_PROVISION,
                api::encode_provision_reply(&api::ProvisionReply { verifier }),
            )
        }
        api::METHOD_FORGET => {
            if !from_accountsd(message) {
                return Err(Error::Errno(-errno::EPERM));
            }
            let args = api::decode_forget_args(body).map_err(parse)?;
            keyd.forget_account(&bounded_text(args.user)?);
            Ok(wire::ok_reply(api::METHOD_FORGET))
        }
        api::METHOD_RESTORE => {
            if !from_accountsd(message) {
                return Err(Error::Errno(-errno::EPERM));
            }
            let args = api::decode_restore_args(body).map_err(parse)?;
            let user = bounded_text(args.user)?;
            keyd.restore(&user, &args.verifier)?;
            Ok(wire::ok_reply(api::METHOD_RESTORE))
        }
        api::METHOD_GENERATE => {
            let owner = caller_uid(message)?;
            let args = api::decode_generate_args(body).map_err(parse)?;
            let kind = bounded_text(args.kind)?;
            let id = keyd
                .generate(&kind, owner)
                .ok_or(Error::Errno(-errno::EINVAL))?;
            reply(
                api::METHOD_GENERATE,
                api::encode_generate_reply(&api::GenerateReply { id }),
            )
        }
        api::METHOD_LIST => {
            let keys = keyd.keys(caller_uid(message)?);
            reply(
                api::METHOD_LIST,
                api::encode_list_reply(&api::ListReply { keys }),
            )
        }
        api::METHOD_STORESECRET => {
            let args = api::decode_store_secret_args(body).map_err(parse)?;
            let owner = secret_owner(message, &args.scope, Op::Store)?;
            let name = bounded_text(args.name)?;
            let secret = bounded_bytes(args.secret, MAX_BYTES)?;
            secrets.put(&mut keyd.entropy, owner, &name, &secret)?;
            Ok(wire::ok_reply(api::METHOD_STORESECRET))
        }
        api::METHOD_DELETESECRET => {
            let args = api::decode_delete_secret_args(body).map_err(parse)?;
            let owner = secret_owner(message, &args.scope, Op::Delete)?;
            let name = bounded_text(args.name)?;
            secrets.delete(&mut keyd.entropy, owner, &name)?;
            Ok(wire::ok_reply(api::METHOD_DELETESECRET))
        }
        api::METHOD_LISTSECRETS => {
            let args = api::decode_list_secrets_args(body).map_err(parse)?;
            let owner = secret_owner(message, &args.scope, Op::List)?;
            reply(
                api::METHOD_LISTSECRETS,
                api::encode_list_secrets_reply(&api::ListSecretsReply {
                    names: secrets.names(owner),
                }),
            )
        }
        api::METHOD_WIFIPMK => {
            let args = api::decode_wifi_pmk_args(body).map_err(parse)?;
            let owner = secret_owner(
                message,
                &args.scope,
                Op::Pmk {
                    owner_uid: args.owner,
                },
            )?;
            let name = bounded_text(args.name)?;
            let ssid = bounded_bytes(args.ssid, secretstore::MAX_SSID)?;
            let pmk = secrets.pmk(&mut keyd.entropy, owner, &name, &ssid)?;
            reply(
                api::METHOD_WIFIPMK,
                api::encode_wifi_pmk_reply(&api::WifiPmkReply { pmk }),
            )
        }
        api::METHOD_PING => Ok(wire::ok_reply(api::METHOD_PING)),
        _ => Err(Error::Errno(-errno::EINVAL)),
    }
}

/// Frame an encoded reply body as a `keyd` parcel of `method`.
fn reply(method: u32, body: Result<Vec<u8>, libmessenger::Error>) -> messenger::Result<Parcel> {
    Ok(wire::parcel(method, body.map_err(Error::Parcel)?))
}

/// The uid of the Messenger sender, from its kernel-stamped credentials.
///
/// `keyd` holds `CAP_SETUID`, which is what lets it read another task's
/// credential block. An unreadable block is refused rather than guessed: a key
/// must never end up owned by (or usable by) the wrong uid.
fn caller_uid(message: &Message) -> messenger::Result<u32> {
    Ok(message.caller().uid)
}

/// Whose secret `op` on `scope` is about, if the sender may do it
/// (`secretstore::authorize`: the sender's kernel-stamped uid, label and
/// session decide, never uid 0 or a capability).
fn secret_owner(message: &Message, scope: &str, op: Op) -> messenger::Result<Owner> {
    let caller = message.caller();
    let caller = Caller {
        uid: caller.uid,
        label_id: caller.label_id,
        session: caller.session,
    };
    authorize(caller, scope, op).map_err(denied_error)
}

/// A text field within the accepted length.
fn bounded_text(text: String) -> messenger::Result<String> {
    if text.len() > MAX_TEXT {
        return Err(Error::Errno(-errno::E2BIG));
    }
    Ok(text)
}

/// A bytes field within `max`.
fn bounded_bytes(bytes: Vec<u8>, max: usize) -> messenger::Result<Vec<u8>> {
    if bytes.len() > max {
        return Err(Error::Errno(-errno::E2BIG));
    }
    Ok(bytes)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
