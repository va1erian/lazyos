//! The named secrets `keyd` keeps (docs/wifi-prerequisites-plan.md WP2): the
//! table is `libs/secretstore`; this file is its disk and its transport
//! rules.
//!
//! Every change is made durable before it is answered, and undone if it
//! cannot be: a secret `keyd` said it keeps is on the disk. The file is
//! written whole to a temporary name, flushed, renamed over the old one and
//! flushed again, so a crash leaves the old file or the new one.
//!
//! The machine key is a 0600 file beside the secrets file, drawn from the
//! entropy pool on first start. That stops a copied secrets file being read;
//! it does not stop root on the volume (no TPM; docs/security-model.md
//! section 8).
//!
//! Failing safe: a secrets file `keyd` cannot open (damaged, truncated,
//! sealed under another key) is refused whole, logged, and moved aside to
//! `secrets.bad` so the next write cannot destroy it; `keyd` serves on with
//! no secrets. A disk that cannot be read at all, or a machine key file of
//! the wrong size, leaves `keyd` serving with persistence off: stores are
//! refused (`EIO`) and no file is touched, rather than overwriting what
//! might be recoverable.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lazyos_crypto::rng::Entropy;
use secretstore::{Denied, Error as StoreError, Owner, Store, MACHINE_KEY_LEN};
use user::files;
use user::messenger::{errno, Error};
use user::sys;

/// `ENOENT` from `user::files`.
const ENOENT: i64 = 2;

pub(crate) struct Secrets {
    store: Store,
    /// `None`: the key or the disk could not be trusted, so nothing is
    /// persisted and nothing may be stored.
    key: Option<[u8; MACHINE_KEY_LEN]>,
}

impl Secrets {
    /// Open the secrets file and the machine key, making the key on first
    /// start. Prints `KEYD:SECRETS:PASS` or `KEYD:SECRETS:FAIL`.
    pub(crate) fn load(entropy: &mut Entropy) -> Secrets {
        let mut secrets = Secrets {
            store: Store::new(),
            key: None,
        };
        match secrets.open(entropy) {
            Ok(count) => sys::write_str(&format!(
                "KEYD:SECRETS:PASS count={count} file={}\n",
                fhs::state::KEYD_SECRETS
            )),
            Err(reason) => sys::write_str(&format!(
                "KEYD:SECRETS:FAIL reason={reason} file={}\n",
                fhs::state::KEYD_SECRETS
            )),
        }
        secrets
    }

    fn open(&mut self, entropy: &mut Entropy) -> Result<usize, String> {
        make_dir(fhs::state::CONF_SVC)?;
        make_dir(fhs::state::KEYD_DIR)?;
        let key = self.machine_key(entropy)?;
        let loaded = match files::read_up_to(fhs::state::KEYD_SECRETS, secretstore::FILE_MAX) {
            Ok(bytes) => Store::open(&bytes, &key),
            Err(ENOENT) => Ok(Store::new()),
            // Unreadable is not the same as damaged: leave it alone.
            Err(code) => return Err(format!("unreadable errno={code}")),
        };
        match loaded {
            Ok(store) => self.store = store,
            Err(error) => {
                // Refuse the file whole and keep it for whoever can use it.
                let moved = files::rename(fhs::state::KEYD_SECRETS, fhs::state::KEYD_SECRETS_BAD);
                // Persist only once the file is out of the way.
                self.key = moved.is_ok().then_some(key);
                return Err(format!("{error} moved_aside={}", moved.is_ok()));
            }
        }
        self.key = Some(key);
        Ok(self.store.len())
    }

    /// The machine key: read it, or make it. A secrets file with no key to
    /// open it is moved aside before a key is made, so a new key never sits
    /// beside a file it cannot open.
    fn machine_key(&mut self, entropy: &mut Entropy) -> Result<[u8; MACHINE_KEY_LEN], String> {
        match files::read_up_to(fhs::state::KEYD_MACHINE_KEY, MACHINE_KEY_LEN + 1) {
            Ok(bytes) => <[u8; MACHINE_KEY_LEN]>::try_from(bytes.as_slice())
                .map_err(|_| String::from("the machine key file is not 32 bytes")),
            Err(ENOENT) => {
                if files::stat(fhs::state::KEYD_SECRETS).is_ok() {
                    let _ = files::rename(fhs::state::KEYD_SECRETS, fhs::state::KEYD_SECRETS_BAD);
                }
                let mut key = [0u8; MACHINE_KEY_LEN];
                entropy.try_rdrand();
                entropy.fill(&mut key);
                write_atomic(
                    fhs::state::KEYD_MACHINE_KEY_NEW,
                    fhs::state::KEYD_MACHINE_KEY,
                    &key,
                )
                .map_err(|code| format!("the machine key could not be written errno={code}"))?;
                sys::write_str("KEYD:SECRETS:KEY created\n");
                Ok(key)
            }
            Err(code) => Err(format!("the machine key is unreadable errno={code}")),
        }
    }

    /// `StoreSecret`.
    pub(crate) fn put(
        &mut self,
        entropy: &mut Entropy,
        owner: Owner,
        name: &str,
        secret: &[u8],
    ) -> Result<(), Error> {
        self.change(entropy, |store| store.put(owner, name, secret))
    }

    /// `DeleteSecret`.
    pub(crate) fn delete(
        &mut self,
        entropy: &mut Entropy,
        owner: Owner,
        name: &str,
    ) -> Result<(), Error> {
        self.change(entropy, |store| store.delete(owner, name))
    }

    /// `ListSecrets`: names only.
    pub(crate) fn names(&self, owner: Owner) -> Vec<String> {
        self.store.names(owner)
    }

    /// `WifiPmk`. A PMK derived now is persisted with the cache; if that
    /// fails the answer still stands (the cache is only a speed-up) and the
    /// next call tries again.
    pub(crate) fn pmk(
        &mut self,
        entropy: &mut Entropy,
        owner: Owner,
        name: &str,
        ssid: &[u8],
    ) -> Result<Vec<u8>, Error> {
        let (pmk, fresh) = self.store.pmk(owner, name, ssid).map_err(store_error)?;
        if fresh {
            if let Err(error) = self.persist(entropy) {
                sys::write_str(&format!("KEYD:SECRETS:CACHE:FAIL {}\n", error.message()));
            }
        }
        Ok(pmk.to_vec())
    }

    /// Apply `change`, make it durable, or put the table back as it was.
    fn change(
        &mut self,
        entropy: &mut Entropy,
        change: impl FnOnce(&mut Store) -> Result<(), StoreError>,
    ) -> Result<(), Error> {
        if self.key.is_none() {
            return Err(Error::Errno(-errno::EIO));
        }
        let before = self.store.clone();
        change(&mut self.store).map_err(store_error)?;
        if let Err(error) = self.persist(entropy) {
            self.store = before;
            return Err(error);
        }
        Ok(())
    }

    fn persist(&mut self, entropy: &mut Entropy) -> Result<(), Error> {
        let key = self.key.ok_or(Error::Errno(-errno::EIO))?;
        let bytes = self.store.seal(&key, &mut |nonce| {
            entropy.try_rdrand();
            entropy.fill(nonce)
        });
        write_atomic(
            fhs::state::KEYD_SECRETS_NEW,
            fhs::state::KEYD_SECRETS,
            &bytes,
        )
        .map_err(|code| {
            sys::write_str(&format!("KEYD:SECRETS:WRITE:FAIL errno={code}\n"));
            Error::Errno(-errno::EIO)
        })
    }
}

/// The errno a table failure becomes.
pub(crate) fn store_error(error: StoreError) -> Error {
    Error::Errno(-match error {
        StoreError::BadName | StoreError::BadSecret | StoreError::BadSsid => errno::EINVAL,
        StoreError::Full => errno::ENOSPC,
        StoreError::NotFound => errno::ENOENT,
    })
}

/// The errno a refused request becomes.
pub(crate) fn denied_error(denied: Denied) -> Error {
    Error::Errno(-match denied {
        Denied::Perm => errno::EPERM,
        Denied::Invalid => errno::EINVAL,
    })
}

/// Create `path` (0700) unless it is there.
fn make_dir(path: &str) -> Result<(), String> {
    match files::stat(path) {
        Ok(_) => Ok(()),
        Err(_) => files::mkdir(path)
            .and_then(|()| files::chmod(path, 0o700))
            .map_err(|code| format!("{path} could not be made errno={code}")),
    }
}

/// Write `bytes` to `tmp` (0600, in a 0700 directory), flush it, rename it
/// over `target` and flush again. Removes `tmp` on failure.
fn write_atomic(tmp: &str, target: &str, bytes: &[u8]) -> Result<(), i64> {
    let result = files::write_large(tmp, bytes)
        .and_then(|()| files::chmod(tmp, 0o600))
        .and_then(|()| files::fsync(tmp))
        .and_then(|()| files::rename(tmp, target))
        .and_then(|()| files::fsync(target));
    if result.is_err() {
        let _ = files::remove(tmp);
    }
    result
}
