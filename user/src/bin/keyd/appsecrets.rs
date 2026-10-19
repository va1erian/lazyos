//! Apps' own secrets (`os.lazy.keyd.appsecret.v1`, docs/ai-agent-plan.md
//! section 5.3 "P2b"): the table is `secretstore::AppStore`; this file is its
//! disk, the same way `secrets.rs` is the named secrets'.
//!
//! The file (`appsecrets`) is sealed under the machine key `secrets.rs` owns,
//! written whole to a temporary name, flushed, renamed and flushed again, and
//! every change is made durable before it is answered (and undone if it cannot
//! be). A file `keyd` cannot open (damaged, a version this build does not
//! know, sealed under another key) is refused whole, logged and moved aside to
//! `appsecrets.bad`; `keyd` serves on with no app secrets. If the machine key
//! or the disk could not be trusted, persistence is off and a `Put` is `EIO`.
//!
//! No secret is ever in a trace, a log or an error text: this file prints
//! counts and an errno, never a name, an app or a value.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lazyos_crypto::rng::Entropy;
use secretstore::{AppError, AppStore, MACHINE_KEY_LEN};
use user::files;
use user::messenger::{errno, Error};
use user::trace;

use crate::secrets::{write_atomic, ENOENT};

pub(crate) struct AppSecrets {
    store: AppStore,
    /// `None`: nothing is persisted and nothing may be stored.
    key: Option<[u8; MACHINE_KEY_LEN]>,
}

impl AppSecrets {
    /// Open the file under `key` (the machine key, `None` when the disk could
    /// not be trusted). Prints `KEYD:APPSECRETS:PASS` or `...:FAIL`.
    pub(crate) fn load(key: Option<[u8; MACHINE_KEY_LEN]>) -> AppSecrets {
        let mut secrets = AppSecrets {
            store: AppStore::new(),
            key,
        };
        match secrets.open() {
            Ok(count) => trace::line!(
                trace::Channel::Keyd,
                "APPSECRETS:PASS count={count} file={}",
                fhs::state::KEYD_APPSECRETS
            ),
            Err(reason) => trace::line!(
                trace::Channel::Keyd,
                "APPSECRETS:FAIL reason={reason} file={}",
                fhs::state::KEYD_APPSECRETS
            ),
        }
        secrets
    }

    fn open(&mut self) -> Result<usize, String> {
        let Some(key) = self.key else {
            return Err(String::from("persistence is off"));
        };
        let bytes = match files::read_up_to(fhs::state::KEYD_APPSECRETS, secretstore::APP_FILE_MAX)
        {
            Ok(bytes) => bytes,
            Err(ENOENT) => return Ok(0),
            // Unreadable is not the same as damaged: leave it alone, and
            // store nothing over it.
            Err(code) => {
                self.key = None;
                return Err(format!("unreadable errno={code}"));
            }
        };
        match AppStore::open(&bytes, &key) {
            Ok(store) => {
                self.store = store;
                Ok(self.store.len())
            }
            Err(error) => {
                let moved = files::rename(
                    fhs::state::KEYD_APPSECRETS,
                    fhs::state::KEYD_APPSECRETS_BAD,
                );
                // Persist only once the file is out of the way.
                if moved.is_err() {
                    self.key = None;
                }
                Err(format!("{error} moved_aside={}", moved.is_ok()))
            }
        }
    }

    /// `Put`.
    pub(crate) fn put(
        &mut self,
        entropy: &mut Entropy,
        uid: u32,
        app: &str,
        name: &str,
        secret: &[u8],
    ) -> Result<(), Error> {
        // Putting what is already kept writes nothing: an app looping on
        // `Put` must not keep keyd on disk I/O.
        if self.store.get(uid, app, name) == Ok(secret) {
            return Ok(());
        }
        self.change(entropy, |store| store.put(uid, app, name, secret).map(|()| true))
    }

    /// `Get`.
    pub(crate) fn get(&self, uid: u32, app: &str, name: &str) -> Result<Vec<u8>, Error> {
        self.store
            .get(uid, app, name)
            .map(<[u8]>::to_vec)
            .map_err(app_error)
    }

    /// `Delete`.
    pub(crate) fn delete(
        &mut self,
        entropy: &mut Entropy,
        uid: u32,
        app: &str,
        name: &str,
    ) -> Result<(), Error> {
        self.change(entropy, |store| store.delete(uid, app, name).map(|()| true))
    }

    /// `List`: names only.
    pub(crate) fn names(&self, uid: u32, app: &str) -> Vec<String> {
        self.store.names(uid, app)
    }

    /// The apps that hold a secret (what a sweep asks `pkgd` about).
    pub(crate) fn apps(&self) -> Vec<String> {
        self.store.apps()
    }

    /// Delete the secrets of every app in `gone`; returns how many secrets
    /// went. A sweep only deletes: a failed write leaves the table as it was
    /// (the next sweep tries again).
    pub(crate) fn sweep_gone(&mut self, entropy: &mut Entropy, gone: &[String]) -> usize {
        let mut removed = 0;
        let result = self.change(entropy, |store| {
            removed = store.sweep(|app| !gone.iter().any(|g| g == app));
            Ok(removed > 0)
        });
        match result {
            Ok(()) => removed,
            Err(_) => 0,
        }
    }

    /// Apply `change` (true: it changed something), make it durable, or put
    /// the table back as it was.
    fn change(
        &mut self,
        entropy: &mut Entropy,
        change: impl FnOnce(&mut AppStore) -> Result<bool, AppError>,
    ) -> Result<(), Error> {
        let key = self.key.ok_or(Error::Errno(-errno::EIO))?;
        let before = self.store.clone();
        if !change(&mut self.store).map_err(app_error)? {
            return Ok(());
        }
        if let Err(error) = self.persist(entropy, &key) {
            self.store = before;
            return Err(error);
        }
        Ok(())
    }

    fn persist(&mut self, entropy: &mut Entropy, key: &[u8; MACHINE_KEY_LEN]) -> Result<(), Error> {
        let bytes = self.store.seal(key, &mut |nonce| {
            entropy.try_rdrand();
            entropy.fill(nonce)
        });
        write_atomic(
            fhs::state::KEYD_APPSECRETS_NEW,
            fhs::state::KEYD_APPSECRETS,
            &bytes,
        )
        .map_err(|code| {
            trace::line!(trace::Channel::Keyd, "APPSECRETS:WRITE:FAIL errno={code}");
            Error::Errno(-errno::EIO)
        })
    }
}

/// The errno a table failure becomes.
pub(crate) fn app_error(error: AppError) -> Error {
    Error::Errno(-match error {
        AppError::BadName | AppError::BadSecret => errno::EINVAL,
        AppError::Full => errno::ENOSPC,
        AppError::NotFound => errno::ENOENT,
    })
}
