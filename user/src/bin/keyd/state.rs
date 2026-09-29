//! Key table, accounts and the Argon2id-backed operations `keyd` serves.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use lazyos_crypto::{hmac, kdf, rng::Entropy, wrap};
use user::messenger::{errno, keyd as wire, Error};
use user::sys;

/// Live keys the service holds.
pub(crate) const MAX_KEYS: usize = 32;
/// Live keys one uid may hold, so a single client cannot fill the table and
/// lock everyone else out of `GenerateKey`.
pub(crate) const MAX_KEYS_PER_OWNER: usize = 8;
/// Password accounts the service holds.
pub(crate) const MAX_ACCOUNTS: usize = 16;
/// Bytes in a generated symmetric key (256-bit).
pub(crate) const KEY_LEN: usize = 32;
/// Salt bytes for a password verifier.
pub(crate) const SALT_LEN: usize = 16;
/// Longest accepted username or password.
pub(crate) const MAX_TEXT: usize = 64;
/// Longest accepted `Sign` digest, blob or plaintext (mirrors the wire cap).
pub(crate) const MAX_BYTES: usize = wire::MAX_BYTES;

/// One stored symmetric key. `material` is private to this task; no encoder
/// in this file ever reads it except the crypto operations themselves.
pub(crate) struct KeyEntry {
    pub(crate) id: u64,
    /// The uid that generated the key, from the Messenger sender's
    /// kernel-stamped credentials. Only the owner may use or list the key;
    /// key ids are small sequential integers, so without this check any client
    /// could sign, wrap or unwrap with every other client's keys.
    pub(crate) owner: u32,
    pub(crate) kind: &'static str,
    pub(crate) material: [u8; KEY_LEN],
    pub(crate) uses: u64,
    pub(crate) last_use: u64,
}

/// One password account: username, salt and Argon2id verifier.
pub(crate) struct Account {
    pub(crate) user: String,
    pub(crate) salt: [u8; SALT_LEN],
    pub(crate) verifier: [u8; kdf::VERIFIER_LEN],
}

/// The service state: the key table, accounts, entropy pool and the reusable
/// Argon2id arena.
pub(crate) struct Keyd {
    pub(crate) keys: Vec<KeyEntry>,
    pub(crate) accounts: Vec<Account>,
    pub(crate) entropy: Entropy,
    /// One allocation, reused for every verification (the bump allocator never
    /// frees a per-call block; see `libs/crypto::kdf`).
    pub(crate) kdf: kdf::Arena,
    pub(crate) next_id: u64,
}

impl Keyd {
    /// An empty service with its Argon2id arena allocated. `None` when the
    /// arena allocation fails; [`Keyd::seed_entropy`] must still run before
    /// any secret is generated.
    pub(crate) fn new() -> Option<Keyd> {
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
    pub(crate) fn seed_entropy(&mut self) {
        self.entropy.try_rdrand();
        for _ in 0..16 {
            self.entropy.seed_u64(sys::clock());
            self.entropy.mix_timing();
        }
    }

    /// Generate a fresh random key of `kind` for `owner`; `None` for an unknown
    /// kind, a full table, or an owner at its per-uid cap. The material is drawn
    /// from the entropy pool and stored locally.
    pub(crate) fn generate(&mut self, kind: &str, owner: u32) -> Option<u64> {
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
    pub(crate) fn key(&self, id: u64, owner: u32) -> Option<&KeyEntry> {
        self.keys
            .iter()
            .find(|key| key.id == id && key.owner == owner)
    }

    /// Count one use of `id` (called after a successful operation).
    pub(crate) fn touch(&mut self, id: u64) {
        if let Some(key) = self.keys.iter_mut().find(|key| key.id == id) {
            key.uses += 1;
            key.last_use = sys::clock();
        }
    }

    /// Provision the demo account: Argon2id over a fresh salt.
    pub(crate) fn provision_demo_account(&mut self) -> bool {
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
    pub(crate) fn provision(&mut self, user: &str, secret: &str) -> Result<(), Error> {
        if user.is_empty() {
            return Err(Error::Errno(-errno::EINVAL));
        }
        let known = self
            .accounts
            .iter()
            .position(|account| account.user == user);
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
    pub(crate) fn verify(&mut self, user: &str, secret: &str) -> bool {
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
    pub(crate) fn wrap(&mut self, id: u64, owner: u32, plaintext: &[u8]) -> Result<Vec<u8>, Error> {
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
    pub(crate) fn unwrap(&mut self, id: u64, owner: u32, blob: &[u8]) -> Result<Vec<u8>, Error> {
        let Some(key) = self.key(id, owner) else {
            return Err(Error::Errno(-errno::ENOENT));
        };
        let material = key.material;
        let plaintext = wrap::unwrap(&material, blob).map_err(crypto_error)?;
        self.touch(id);
        Ok(plaintext)
    }

    /// HMAC-SHA256 `digest` under `owner`'s key `id`; returns the 32-byte tag.
    pub(crate) fn sign(
        &mut self,
        id: u64,
        owner: u32,
        digest: &[u8],
    ) -> Result<[u8; hmac::TAG_LEN], Error> {
        let Some(key) = self.key(id, owner) else {
            return Err(Error::Errno(-errno::ENOENT));
        };
        let material = key.material;
        let tag = hmac::hmac_sha256(&material, digest);
        self.touch(id);
        Ok(tag)
    }

    /// `len` bytes from the pool, capped by [`MAX_BYTES`].
    pub(crate) fn random(&mut self, len: usize) -> Result<Vec<u8>, Error> {
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
    pub(crate) fn keys(&self, owner: u32) -> Vec<wire::KeyInfo> {
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
    pub(crate) fn forget_keys_owned_by(&mut self, owner: u32) {
        self.keys.retain(|key| key.owner != owner);
    }

    /// Drop `user`'s account, if any. Used once, right after the boot
    /// self-test, to scrub the throwaway account it provisions to exercise
    /// re-provisioning: left in place it would count against
    /// [`MAX_ACCOUNTS`] and be a login-able account no session ever meant to
    /// create.
    pub(crate) fn forget_account(&mut self, user: &str) {
        self.accounts.retain(|account| account.user != user);
    }
}

/// Map a crypto failure onto the friendly errno the client sees.
pub(crate) fn crypto_error(error: lazyos_crypto::Error) -> Error {
    match error {
        // Wrong key and tampered blob are the same answer to the caller.
        lazyos_crypto::Error::BadTag | lazyos_crypto::Error::Malformed => {
            Error::Errno(-errno::EBADMSG)
        }
        _ => Error::Errno(-errno::EINVAL),
    }
}

/// Constant-time byte comparison for verifiers (no early exit).
pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}
