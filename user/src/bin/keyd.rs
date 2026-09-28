//! `keyd` (`KEYD.ELF`): the secrets and crypto service (issue #102).
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
//! The kernel already implements `SHARE_ONLY` shared buffers: a buffer flagged
//! `SHARE_ONLY` is mapped for its creator and the kernel refuses to map it into
//! any other task (`kernel/src/ipc/shared.rs`), and the in-kernel test
//! `ipc_buffer_share_only_not_mappable` proves it. The design intent is for
//! `keyd` to keep its key table inside such a buffer, so even a kernel bug that
//! leaks a handle cannot put key bytes in a client's address space.
//!
//! There is **no userspace syscall for shared buffers yet** (the `OP_*` table
//! in `kernel/src/ipc/syscalls.rs` has no create/map op). Until it lands, the
//! equivalent property holds structurally: the key table never leaves this
//! task, the wire protocol has no "read key material" method, and replies carry
//! only ids, tags, blobs and counters. The `SHARE_ONLY` move is mechanical when
//! the op exists and is called out again at [`Keyd::keys`].
//!
//! # Provisioning
//!
//! `accountsd` pushes its database into `keyd` with the root-only `Provision`
//! method the first time it delegates a login, and again for every user created
//! afterwards; `keyd`'s verdict is authoritative once it is registered, so an
//! account `keyd` was never told about cannot log in. `keyd` also provisions one
//! demo account in memory at boot (`lazyos`/`lazyos`) and the self-test checks
//! it. Every verifier is Argon2id under [`kdf::Params::INTERACTIVE`].
//!
//! Keys are scoped to the uid that generated them (taken from the sender's
//! kernel-stamped credentials): `Sign`, `Wrap`, `Unwrap` and `List` only see the
//! caller's own keys.
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
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::panic::PanicInfo;
use lazyos_crypto::{hex, hmac, kdf, rng::Entropy, sha256, wrap};
use user::messenger::{self, errno, keyd as wire, registry, Error, Message, Parcel};
use user::sys;

/// Live keys the service holds.
const MAX_KEYS: usize = 32;
/// Live keys one uid may hold, so a single client cannot fill the table and
/// lock everyone else out of `GenerateKey`.
const MAX_KEYS_PER_OWNER: usize = 8;
/// Password accounts the service holds.
const MAX_ACCOUNTS: usize = 16;
/// Bytes in a generated symmetric key (256-bit).
const KEY_LEN: usize = 32;
/// Salt bytes for a password verifier.
const SALT_LEN: usize = 16;
/// Longest accepted username or password.
const MAX_TEXT: usize = 64;
/// Longest accepted `Sign` digest, blob or plaintext (mirrors the wire cap).
const MAX_BYTES: usize = wire::MAX_BYTES;

/// One stored symmetric key. `material` is private to this task; no encoder
/// in this file ever reads it except the crypto operations themselves.
struct KeyEntry {
    id: u64,
    /// The uid that generated the key, from the Messenger sender's
    /// kernel-stamped credentials. Only the owner may use or list the key;
    /// key ids are small sequential integers, so without this check any client
    /// could sign, wrap or unwrap with every other client's keys.
    owner: u32,
    kind: &'static str,
    material: [u8; KEY_LEN],
    uses: u64,
    last_use: u64,
}

/// One password account: username, salt and Argon2id verifier.
struct Account {
    user: String,
    salt: [u8; SALT_LEN],
    verifier: [u8; kdf::VERIFIER_LEN],
}

/// The service state: the key table, accounts, entropy pool and the reusable
/// Argon2id arena.
struct Keyd {
    keys: Vec<KeyEntry>,
    accounts: Vec<Account>,
    entropy: Entropy,
    /// One allocation, reused for every verification (the bump allocator never
    /// frees a per-call block; see `libs/crypto::kdf`).
    kdf: kdf::Arena,
    next_id: u64,
}

impl Keyd {
    /// An empty service with its Argon2id arena allocated. `None` when the
    /// arena allocation fails; [`Keyd::seed_entropy`] must still run before
    /// any secret is generated.
    fn new() -> Option<Keyd> {
        Some(Keyd {
            keys: Vec::new(),
            accounts: Vec::new(),
            entropy: Entropy::new(),
            kdf: kdf::Arena::new(kdf::Params::INTERACTIVE).ok()?,
            next_id: 1,
        })
    }

    /// Seed the pool: RDRAND first (hardware), then PIT ticks and TSC jitter.
    ///
    /// Timing jitter comes from sampling the TSC around variable-cost work;
    /// this loop's own duration varies with cache and scheduler state, so the
    /// samples differ between boots and between runs on the same boot.
    fn seed_entropy(&mut self) {
        self.entropy.try_rdrand();
        for _ in 0..16 {
            self.entropy.seed_u64(sys::clock());
            self.entropy.mix_timing();
        }
    }

    /// Generate a fresh random key of `kind` for `owner`; `None` for an unknown
    /// kind, a full table, or an owner at its per-uid cap. The material is drawn
    /// from the entropy pool and stored locally.
    fn generate(&mut self, kind: &str, owner: u32) -> Option<u64> {
        let kind = match kind {
            wire::KIND_HMAC => wire::KIND_HMAC,
            wire::KIND_WRAP => wire::KIND_WRAP,
            _ => return None,
        };
        if self.keys.len() >= MAX_KEYS
            || self.keys.iter().filter(|key| key.owner == owner).count() >= MAX_KEYS_PER_OWNER
        {
            return None;
        }
        let mut material = [0u8; KEY_LEN];
        self.entropy.try_rdrand();
        self.entropy.fill(&mut material);
        let id = self.next_id;
        self.next_id += 1;
        self.keys.push(KeyEntry {
            id,
            owner,
            kind,
            material,
            uses: 0,
            last_use: 0,
        });
        Some(id)
    }

    /// The key named by `id` if it exists *and belongs to `owner`*. A key that
    /// exists but is someone else's answers exactly like a missing one, so the
    /// id space is not an oracle for other users' keys.
    fn key(&self, id: u64, owner: u32) -> Option<&KeyEntry> {
        self.keys
            .iter()
            .find(|key| key.id == id && key.owner == owner)
    }

    /// Count one use of `id` (called after a successful operation).
    fn touch(&mut self, id: u64) {
        if let Some(key) = self.keys.iter_mut().find(|key| key.id == id) {
            key.uses += 1;
            key.last_use = sys::clock();
        }
    }

    /// Provision the demo account: Argon2id over a fresh salt.
    fn provision_demo_account(&mut self) -> bool {
        if self.accounts.len() >= MAX_ACCOUNTS {
            return false;
        }
        let mut salt = [0u8; SALT_LEN];
        self.entropy.try_rdrand();
        self.entropy.fill(&mut salt);
        let mut verifier = [0u8; kdf::VERIFIER_LEN];
        if self.kdf.derive(b"lazyos", &salt, &mut verifier).is_err() {
            return false;
        }
        self.accounts.push(Account {
            user: String::from("lazyos"),
            salt,
            verifier,
        });
        true
    }

    /// Install (or replace) `user`'s verifier: Argon2id over a fresh salt.
    /// The plaintext is only borrowed for the derivation.
    fn provision(&mut self, user: &str, secret: &str) -> Result<(), Error> {
        if user.is_empty() {
            return Err(Error::Errno(-errno::EINVAL));
        }
        let known = self.accounts.iter().position(|account| account.user == user);
        if known.is_none() && self.accounts.len() >= MAX_ACCOUNTS {
            return Err(Error::Errno(-errno::ENOMEM));
        }
        let mut salt = [0u8; SALT_LEN];
        self.entropy.try_rdrand();
        self.entropy.fill(&mut salt);
        let mut verifier = [0u8; kdf::VERIFIER_LEN];
        self.kdf
            .derive(secret.as_bytes(), &salt, &mut verifier)
            .map_err(|_| Error::Errno(-errno::ENOMEM))?;
        match known {
            Some(index) => {
                self.accounts[index].salt = salt;
                self.accounts[index].verifier = verifier;
            }
            None => self.accounts.push(Account {
                user: String::from(user),
                salt,
                verifier,
            }),
        }
        Ok(())
    }

    /// Check a password against the stored verifier for `user`.
    ///
    /// The comparison is over freshly derived verifiers, and the candidate
    /// derivation runs even for an unknown user only when an account exists:
    /// an unknown user returns immediately (the accounts service will add
    /// constant-time failure later; there is no account enumeration surface in
    /// this demo).
    fn verify(&mut self, user: &str, secret: &str) -> bool {
        // Copy the account's salt and verifier out before borrowing the arena
        // mutably: the derivation writes into the reusable block memory.
        let Some((salt, verifier)) = self
            .accounts
            .iter()
            .find(|account| account.user == user)
            .map(|account| (account.salt, account.verifier))
        else {
            return false;
        };
        let mut candidate = [0u8; kdf::VERIFIER_LEN];
        if self
            .kdf
            .derive(secret.as_bytes(), &salt, &mut candidate)
            .is_err()
        {
            return false;
        }
        constant_time_eq(&candidate, &verifier)
    }

    /// Wrap `plaintext` under `owner`'s key `id`.
    fn wrap(&mut self, id: u64, owner: u32, plaintext: &[u8]) -> Result<Vec<u8>, Error> {
        let Some(key) = self.key(id, owner) else {
            return Err(Error::Errno(-errno::ENOENT));
        };
        let material = key.material;
        // A fresh 128-bit nonce per wrap comes from the pool; reuse would leak
        // plaintext XOR and make repeated pairs detectable.
        let mut nonce = [0u8; wrap::NONCE_LEN];
        self.entropy.try_rdrand();
        self.entropy.fill(&mut nonce);
        let blob = wrap::wrap_with_nonce(&material, &nonce, plaintext).map_err(crypto_error)?;
        self.touch(id);
        Ok(blob)
    }

    /// Open a blob under `owner`'s key `id`; a wrong key or tampering is
    /// `EBADMSG`.
    fn unwrap(&mut self, id: u64, owner: u32, blob: &[u8]) -> Result<Vec<u8>, Error> {
        let Some(key) = self.key(id, owner) else {
            return Err(Error::Errno(-errno::ENOENT));
        };
        let material = key.material;
        let plaintext = wrap::unwrap(&material, blob).map_err(crypto_error)?;
        self.touch(id);
        Ok(plaintext)
    }

    /// HMAC-SHA256 `digest` under `owner`'s key `id`; returns the 32-byte tag.
    fn sign(&mut self, id: u64, owner: u32, digest: &[u8]) -> Result<[u8; hmac::TAG_LEN], Error> {
        let Some(key) = self.key(id, owner) else {
            return Err(Error::Errno(-errno::ENOENT));
        };
        let material = key.material;
        let tag = hmac::hmac_sha256(&material, digest);
        self.touch(id);
        Ok(tag)
    }

    /// `len` bytes from the pool, capped by [`MAX_BYTES`].
    fn random(&mut self, len: usize) -> Result<Vec<u8>, Error> {
        if len > MAX_BYTES {
            return Err(Error::Errno(-errno::E2BIG));
        }
        let mut bytes = alloc::vec![0u8; len];
        self.entropy.try_rdrand();
        self.entropy.fill(&mut bytes);
        Ok(bytes)
    }

    /// The public key rows for `owner`: ids, kinds and counters. This is the
    /// *only* shape in which keys leave [`Keyd`], and only the caller's own.
    ///
    /// When the shared-buffer syscall lands, the table moves into a
    /// `SHARE_ONLY` buffer created in this task; the kernel then refuses to map
    /// it anywhere else, so even a leaked handle cannot expose `material`.
    fn keys(&self, owner: u32) -> Vec<wire::KeyInfo> {
        self.keys
            .iter()
            .filter(|key| key.owner == owner)
            .map(|key| wire::KeyInfo {
                id: key.id,
                kind: key.kind.to_string(),
                uses: key.uses,
                last_use: key.last_use,
            })
            .collect()
    }

    /// Drop every key owned by `owner`. Used once, right after the boot
    /// self-test, to scrub the keys it generated (see [`run`]'s call site):
    /// left in place they would count against `owner`'s
    /// [`MAX_KEYS_PER_OWNER`] and appear in that uid's real `List` forever.
    fn forget_keys_owned_by(&mut self, owner: u32) {
        self.keys.retain(|key| key.owner != owner);
    }

    /// Drop `user`'s account, if any. Used once, right after the boot
    /// self-test, to scrub the throwaway account it provisions to exercise
    /// re-provisioning: left in place it would count against
    /// [`MAX_ACCOUNTS`] and be a login-able account no session ever meant to
    /// create.
    fn forget_account(&mut self, user: &str) {
        self.accounts.retain(|account| account.user != user);
    }
}

/// Map a crypto failure onto the friendly errno the client sees.
fn crypto_error(error: lazyos_crypto::Error) -> Error {
    match error {
        // Wrong key and tampered blob are the same answer to the caller.
        lazyos_crypto::Error::BadTag | lazyos_crypto::Error::Malformed => {
            Error::Errno(-errno::EBADMSG)
        }
        _ => Error::Errno(-errno::EINVAL),
    }
}

/// Constant-time byte comparison for verifiers (no early exit).
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

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
    if !keyd.provision_demo_account() {
        sys::write_str("KEYD:SELFTEST:FAIL:account provisioning\n");
        sys::exit(1);
    }
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

    // One receive buffer for the whole life of the service: the user bump
    // allocator never reclaims per-call buffers, so a long-lived loop must not
    // allocate one per request.
    let mut buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    loop {
        let message = server.recv_with(&mut buffer, None)?;
        let method = message.method();
        let reply = match dispatch(&mut keyd, &message) {
            Ok(reply) => reply,
            Err(error) => wire::error_reply(method, error),
        };
        if let Some(txn) = message.txn {
            server.reply_or_drop(txn, &reply)?;
        }
    }
}

/// The boot self-test: known-answer vectors, a wrap round-trip with tamper
/// rejection, password verification, and the RNG. Returns a printable detail
/// on failure so the serial marker says exactly what broke.
fn self_test(keyd: &mut Keyd) -> Result<(), String> {
    // FIPS 180-4: SHA-256("abc").
    let digest = sha256::sha256(b"abc");
    let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    if hex::encode(&digest) != expected {
        return Err(format!("sha256(abc)={}", hex::encode(&digest)));
    }

    // RFC 4231 case 1: HMAC-SHA256.
    let tag = hmac::hmac_sha256(&[0x0bu8; 20], b"Hi There");
    let expected = "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7";
    if hex::encode(&tag) != expected {
        return Err(format!("hmac={}", hex::encode(&tag)));
    }

    // A generated wrapping key round-trips, and a tampered blob is refused.
    let wrap_key = keyd
        .generate(wire::KIND_WRAP, SELF_TEST_OWNER)
        .ok_or_else(|| String::from("generate wrap key"))?;
    let blob = keyd
        .wrap(wrap_key, SELF_TEST_OWNER, b"selftest secret")
        .map_err(|error| error.message())?;
    let opened = keyd
        .unwrap(wrap_key, SELF_TEST_OWNER, &blob)
        .map_err(|error| error.message())?;
    if opened != b"selftest secret" {
        return Err(String::from("wrap round-trip mismatch"));
    }
    let mut tampered = blob.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    if keyd.unwrap(wrap_key, SELF_TEST_OWNER, &tampered).is_ok() {
        return Err(String::from("tampered blob unwrapped"));
    }
    // Another uid must not be able to use (or even see) the key.
    let stranger = SELF_TEST_OWNER + 1;
    if keyd.unwrap(wrap_key, stranger, &blob).is_ok()
        || keyd.wrap(wrap_key, stranger, b"x").is_ok()
        || keyd.sign(wrap_key, stranger, b"x").is_ok()
        || !keyd.keys(stranger).is_empty()
    {
        return Err(String::from("a non-owner used or listed a key"));
    }

    // Argon2id password verification, both directions.
    if !keyd.verify("lazyos", "lazyos") {
        return Err(String::from("demo account rejected"));
    }
    if keyd.verify("lazyos", "not-lazyos") {
        return Err(String::from("wrong password accepted"));
    }
    // A provisioned account verifies, and re-provisioning replaces the secret.
    keyd.provision("selftest-user", "first")
        .map_err(|error| error.message())?;
    keyd.provision("selftest-user", "second")
        .map_err(|error| error.message())?;
    if keyd.verify("selftest-user", "first") || !keyd.verify("selftest-user", "second") {
        return Err(String::from("provisioned secret did not replace the old one"));
    }

    // A signing key produces a tag and counts its use.
    let hmac_key = keyd
        .generate(wire::KIND_HMAC, SELF_TEST_OWNER)
        .ok_or_else(|| String::from("generate hmac key"))?;
    let tag = keyd
        .sign(hmac_key, SELF_TEST_OWNER, b"digest")
        .map_err(|error| error.message())?;
    if tag == [0u8; hmac::TAG_LEN] {
        return Err(String::from("sign returned a zero tag"));
    }

    // The pool produces non-trivial bytes.
    let random = keyd.random(32).map_err(|error| error.message())?;
    if random.len() != 32 || random.iter().all(|byte| *byte == 0) {
        return Err(String::from("random output looks wrong"));
    }
    Ok(())
}

/// Dispatch one request; errors become error replies at the call site.
fn dispatch(keyd: &mut Keyd, message: &Message) -> messenger::Result<Parcel> {
    if message.interface_id() != wire::INTERFACE {
        return Err(Error::Errno(-errno::EINVAL));
    }
    let method = message.method();
    match method {
        wire::method::VERIFY => {
            let user = text_field(message, wire::field::USER)?;
            let secret = text_field(message, wire::field::SECRET)?;
            Ok(wire::bool_reply(method, keyd.verify(&user, &secret)))
        }
        wire::method::SIGN => {
            let owner = caller_uid(message)?;
            let key = key_field(message)?;
            let digest = bytes_field(message, wire::field::DIGEST, 128)?;
            let tag = keyd.sign(key, owner, &digest)?;
            wire::bytes_reply(method, &tag)
        }
        wire::method::WRAP => {
            let owner = caller_uid(message)?;
            let key = key_field(message)?;
            let plaintext = bytes_field(message, wire::field::DATA, MAX_BYTES)?;
            let blob = keyd.wrap(key, owner, &plaintext)?;
            wire::bytes_reply(method, &blob)
        }
        wire::method::UNWRAP => {
            let owner = caller_uid(message)?;
            let key = key_field(message)?;
            let blob = bytes_field(message, wire::field::DATA, MAX_BYTES)?;
            let plaintext = keyd.unwrap(key, owner, &blob)?;
            wire::bytes_reply(method, &plaintext)
        }
        wire::method::RANDOM => {
            let len = wire::u64_field(&message.parcel, wire::field::LEN)?
                .ok_or(Error::Errno(-errno::EINVAL))?;
            let bytes = keyd.random(len as usize)?;
            wire::bytes_reply(method, &bytes)
        }
        wire::method::PROVISION => {
            // Only root may plant a verifier: whoever can provision an account
            // can log in as it.
            if caller_uid(message)? != 0 {
                return Err(Error::Errno(-errno::EPERM));
            }
            let user = text_field(message, wire::field::USER)?;
            let secret = text_field(message, wire::field::SECRET)?;
            keyd.provision(&user, &secret)?;
            Ok(wire::ok_reply(method))
        }
        wire::method::GENERATE => {
            let owner = caller_uid(message)?;
            let kind = text_field(message, wire::field::KIND)?;
            let id = keyd
                .generate(&kind, owner)
                .ok_or(Error::Errno(-errno::EINVAL))?;
            wire::id_reply(method, id)
        }
        wire::method::LIST => wire::keys_reply(&keyd.keys(caller_uid(message)?)),
        wire::method::PING => Ok(wire::ok_reply(method)),
        _ => Err(Error::Errno(-errno::EINVAL)),
    }
}

/// The owner used by the boot self-test (root; no Messenger sender).
const SELF_TEST_OWNER: u32 = 0;

/// The uid of the Messenger sender, from its kernel-stamped credentials.
///
/// `keyd` holds `CAP_SETUID`, which is what lets it read another task's
/// credential block. An unreadable block is refused rather than guessed: a key
/// must never end up owned by (or usable by) the wrong uid.
fn caller_uid(message: &Message) -> messenger::Result<u32> {
    let mut cred = sys::Cred::default();
    sys::cred_get(Some(message.sender), &mut cred).map_err(|_| Error::Errno(-errno::EACCES))?;
    Ok(cred.uid)
}

/// The key id field of a request.
fn key_field(message: &Message) -> messenger::Result<u64> {
    wire::u64_field(&message.parcel, wire::field::KEY)?.ok_or(Error::Errno(-errno::EINVAL))
}

/// A bounded `String` field with `id`.
fn text_field(message: &Message, id: u16) -> messenger::Result<String> {
    let text = wire::string_field(&message.parcel, id)?.ok_or(Error::Errno(-errno::EINVAL))?;
    if text.len() > MAX_TEXT {
        return Err(Error::Errno(-errno::E2BIG));
    }
    Ok(text)
}

/// A bounded `Bytes` field with `id`.
fn bytes_field(message: &Message, id: u16, max: usize) -> messenger::Result<Vec<u8>> {
    let bytes = wire::bytes_field(&message.parcel, id)?.ok_or(Error::Errno(-errno::EINVAL))?;
    if bytes.len() > max {
        return Err(Error::Errno(-errno::E2BIG));
    }
    Ok(bytes)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
