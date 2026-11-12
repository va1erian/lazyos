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
use libmessenger::server::{Caller, Fault};
use user::messenger::{self, errno, keyd as wire, registry, Error};
use user::sys;
use user::trace;

#[path = "keyd/appsecrets.rs"]
mod appsecrets;
#[path = "keyd/appserve.rs"]
mod appserve;
#[path = "keyd/secrets.rs"]
mod secrets;
#[path = "keyd/selftest.rs"]
mod selftest;
#[path = "keyd/state.rs"]
mod state;
#[path = "keyd/sweep.rs"]
mod sweep;

use appsecrets::AppSecrets;
use secrets::{denied_error, Secrets};
use secretstore::{authorize, Op, Owner};
use selftest::{self_test, SELF_TEST_OWNER};
use state::{Keyd, MAX_BYTES, MAX_TEXT};
use sweep::Sweep;
use user::messenger::keyd::appsecret;

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
        trace::line!(trace::Channel::Keyd, "SELFTEST:FAIL:no entropy source");
        sys::exit(1);
    }
    load_db(&mut keyd);
    let mut secrets = Secrets::load(&mut keyd.entropy);
    let mut app_secrets = AppSecrets::load(secrets.machine_key_ref());
    let mut sweep = Sweep::new();
    match self_test(&mut keyd) {
        Ok(()) => trace::line!(trace::Channel::Keyd, "SELFTEST:PASS"),
        // Keep serving: some operations may still be usable, and `init` would
        // restart a service that exits, hiding the failure in a crash loop.
        Err(detail) => trace::line!(trace::Channel::Keyd, "SELFTEST:FAIL:{detail}"),
    }
    // Scrub the self-test's own keys and account before this instance starts
    // taking real requests: they must not consume the serving instance's
    // MAX_ACCOUNTS/MAX_KEYS_PER_OWNER capacity or be visible to a real client.
    // Whatever the self-test reached (pass or fail), root has generated no
    // keys but its own at this point, so this cannot remove anything else's.
    keyd.forget_keys_owned_by(SELF_TEST_OWNER);
    keyd.forget_account("selftest-user");

    let (published, server) = messenger::create_pair()?;
    let interfaces = [wire::INTERFACE, appsecret::INTERFACE];
    registry::register(wire::NAME, &published, &interfaces, 0)?;
    registry::register(appsecret::NAME, &published, &interfaces, 0)?;
    trace::line!(trace::Channel::Keyd, "READY");
    // Serving: what waits for this service may start (init.Ready, P7.3).
    user::messenger::services::init::notify_ready();

    // One receive buffer for the whole life of the service: the user bump
    // allocator never reclaims per-call buffers, so a long-lived loop must not
    // allocate one per request.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    loop {
        // Wake for the sweep of removed apps' secrets (sweep.rs).
        let message = match server.recv_with(&mut buffer, Some(sweep.deadline())) {
            Ok(message) => message,
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => {
                sweep.tick(&mut app_secrets, &mut keyd.entropy);
                continue;
            }
            Err(error) => return Err(error),
        };
        sweep.tick(&mut app_secrets, &mut keyd.entropy);
        let reply = if message.interface_id() == appsecret::INTERFACE {
            let mut app = appserve::Handler {
                entropy: &mut keyd.entropy,
                secrets: &mut app_secrets,
            };
            appsecret::wire::dispatch(&mut app, &message)
        } else {
            let mut handler = Handler {
                keyd: &mut keyd,
                secrets: &mut secrets,
            };
            api::dispatch(&mut handler, &message)
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
        Ok(rows) => trace::line!(trace::Channel::Keyd, "SHADOW:PASS rows={rows} file={path}"),
        Err(reason) => trace::line!(
            trace::Channel::Keyd,
            "SHADOW:FAIL reason={reason} file={path}; no account can log in"
        ),
    }
}

/// Whether the request comes from the accounts service: its system uid,
/// unlabelled and outside any session (kernel-stamped, so unforgeable).
fn from_accountsd(cx: &Caller) -> bool {
    cx.uid == accountdb::ACCOUNTS_UID && cx.label_id == 0 && cx.session == 0
}

/// Refuse a request that does not come from the accounts service.
fn require_accountsd(cx: &Caller) -> Result<(), Fault> {
    if from_accountsd(cx) {
        Ok(())
    } else {
        Err(Error::Errno(-errno::EPERM).into())
    }
}

/// The key and secret state, answering `os.lazy.keyd.v1` (the generated server).
struct Handler<'a> {
    keyd: &'a mut Keyd,
    secrets: &'a mut Secrets,
}

impl api::Server for Handler<'_> {
    fn verify(&mut self, cx: &Caller, user: String, secret: String) -> Result<bool, Fault> {
        // A password check is `accountsd`'s alone: it slows guessers
        // (`Authenticate`'s brake), and an open `Verify` would be a way
        // around that brake for any session (review of #659, H2).
        require_accountsd(cx)?;
        Ok(self.keyd.verify(&bounded_text(user)?, &bounded_text(secret)?))
    }

    fn sign(&mut self, cx: &Caller, key: u64, digest: Vec<u8>) -> Result<Vec<u8>, Fault> {
        let digest = bounded_bytes(digest, 128)?;
        Ok(self.keyd.sign(key, cx.uid, &digest)?.to_vec())
    }

    fn wrap(&mut self, cx: &Caller, key: u64, plaintext: Vec<u8>) -> Result<Vec<u8>, Fault> {
        let plaintext = bounded_bytes(plaintext, MAX_BYTES)?;
        Ok(self.keyd.wrap(key, cx.uid, &plaintext)?)
    }

    fn unwrap(&mut self, cx: &Caller, key: u64, blob: Vec<u8>) -> Result<Vec<u8>, Fault> {
        let blob = bounded_bytes(blob, MAX_BYTES)?;
        Ok(self.keyd.unwrap(key, cx.uid, &blob)?)
    }

    fn random(&mut self, _: &Caller, len: u64) -> Result<Vec<u8>, Fault> {
        Ok(self.keyd.random(len as usize)?)
    }

    fn provision(&mut self, cx: &Caller, user: String, secret: String) -> Result<String, Fault> {
        // Whoever can plant a verifier can log in as that account: the
        // accounts service alone, by its identity (U1). No capability or
        // uid is enough on its own.
        require_accountsd(cx)?;
        Ok(self.keyd.provision(&bounded_text(user)?, &bounded_text(secret)?)?)
    }

    fn forget(&mut self, cx: &Caller, user: String) -> Result<(), Fault> {
        require_accountsd(cx)?;
        self.keyd.forget_account(&bounded_text(user)?);
        Ok(())
    }

    fn restore(&mut self, cx: &Caller, user: String, verifier: String) -> Result<(), Fault> {
        require_accountsd(cx)?;
        Ok(self.keyd.restore(&bounded_text(user)?, &verifier)?)
    }

    fn generate(&mut self, cx: &Caller, kind: String) -> Result<u64, Fault> {
        let kind = bounded_text(kind)?;
        let id = self
            .keyd
            .generate(&kind, cx.uid)
            .ok_or(Error::Errno(-errno::EINVAL))?;
        Ok(id)
    }

    fn list(&mut self, cx: &Caller) -> Result<Vec<api::KeyInfo>, Fault> {
        Ok(self.keyd.keys(cx.uid))
    }

    fn store_secret(
        &mut self,
        cx: &Caller,
        scope: String,
        name: String,
        secret: Vec<u8>,
    ) -> Result<(), Fault> {
        let owner = secret_owner(cx, &scope, &name, Op::Store)?;
        let name = bounded_text(name)?;
        let secret = bounded_bytes(secret, MAX_BYTES)?;
        Ok(self.secrets.put(&mut self.keyd.entropy, owner, &name, &secret)?)
    }

    fn delete_secret(&mut self, cx: &Caller, scope: String, name: String) -> Result<(), Fault> {
        let owner = secret_owner(cx, &scope, &name, Op::Delete)?;
        let name = bounded_text(name)?;
        Ok(self.secrets.delete(&mut self.keyd.entropy, owner, &name)?)
    }

    fn list_secrets(&mut self, cx: &Caller, scope: String) -> Result<Vec<String>, Fault> {
        let owner = secret_owner(cx, &scope, "", Op::List)?;
        Ok(self.secrets.names(owner))
    }

    fn store_secret_for(
        &mut self,
        cx: &Caller,
        scope: String,
        name: String,
        secret: Vec<u8>,
        owner: u32,
    ) -> Result<(), Fault> {
        let op = Op::StoreFor { owner_uid: owner };
        let owner = secret_owner(cx, &scope, &name, op)?;
        let name = bounded_text(name)?;
        let secret = bounded_bytes(secret, MAX_BYTES)?;
        Ok(self.secrets.put(&mut self.keyd.entropy, owner, &name, &secret)?)
    }

    fn delete_secret_for(
        &mut self,
        cx: &Caller,
        scope: String,
        name: String,
        owner: u32,
    ) -> Result<(), Fault> {
        let op = Op::DeleteFor { owner_uid: owner };
        let owner = secret_owner(cx, &scope, &name, op)?;
        let name = bounded_text(name)?;
        Ok(self.secrets.delete(&mut self.keyd.entropy, owner, &name)?)
    }

    fn wifi_pmk(
        &mut self,
        cx: &Caller,
        scope: String,
        name: String,
        ssid: Vec<u8>,
        owner: u32,
    ) -> Result<Vec<u8>, Fault> {
        let op = Op::Pmk { owner_uid: owner };
        let owner = secret_owner(cx, &scope, &name, op)?;
        let name = bounded_text(name)?;
        let ssid = bounded_bytes(ssid, secretstore::MAX_SSID)?;
        Ok(self.secrets.pmk(&mut self.keyd.entropy, owner, &name, &ssid)?)
    }

    fn ping(&mut self, _: &Caller) -> Result<(), Fault> {
        Ok(())
    }
}

/// Whose secret `op` on `scope` and `name` is about, if the sender may do it
/// (`secretstore::authorize`: the sender's kernel-stamped uid, label and
/// session decide, never uid 0 or a capability; `wlanmd` names Wi-Fi
/// profiles only).
fn secret_owner(cx: &Caller, scope: &str, name: &str, op: Op) -> messenger::Result<Owner> {
    let caller = secretstore::Caller {
        uid: cx.uid,
        label_id: cx.label_id,
        session: cx.session,
    };
    authorize(caller, scope, name, op).map_err(denied_error)
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
